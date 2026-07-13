// A minimal egress broker in Go — a drop-in stand-in for fabricd, speaking the runlet-wire
// QUIC protocol. It proves the second half of "your io can be anything": not just the
// backend, but the *broker* that resolves logical names to backends can be any language, as
// long as it speaks the wire this repo owns (crates/runlet-wire).
//
// What a broker must implement (see crates/runlet-wire/src/{wire.rs,quic.rs}):
//
//	Transport: QUIC + TLS 1.3, ALPN token "fabricd/1", one bidi stream per box request.
//	           The box PINS the server cert by SHA-256(DER) fingerprint — no CA. This
//	           program generates a self-signed cert on boot and prints the pin to paste
//	           into the box's `broker_quic.server_cert_pin`.
//
//	Framing:   each frame = u32 little-endian length + that many bytes of JSON.
//
//	Session:   Init (once) -> Ack | InitError, then N x (Call -> Reply), then Drain -> Metrics.
//	           Enums are serde-externally-tagged: objects like {"Init":{…}}, {"Call":{…}},
//	           {"Reply":{"Ok":"<json string>"}}, and unit variants as bare strings ("Drain").
//
// It holds the credentials the box does not: it validates the box's auth token, then resolves
// each logical name (`cache`, `orders`) to an in-memory backend. Swap those backends for real
// Postgres/Redis/etc. drivers and you have a production broker.
package main

import (
	"bytes"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"math/big"
	"os"
	"sync"
	"time"

	"github.com/quic-go/quic-go"
)

const (
	alpn        = "fabricd/1" // must match crates/runlet-wire/src/quic.rs
	maxFrame    = 64 << 20    // MAX_FRAME_BYTES
	defaultAddr = "127.0.0.1:4443"
)

// ---- wire types (mirrors of the serde structs in crates/runlet-wire/src/wire.rs) ----------

type wireInit struct {
	Resources []string `json:"resources"`
	TimeoutMs uint64   `json:"timeout_ms"`
	Tenant    *string  `json:"tenant,omitempty"`
	Actor     *string  `json:"actor,omitempty"`
	Token     *string  `json:"token,omitempty"`
}

type wireCall struct {
	Name    string `json:"name"`
	Action  string `json:"action"`
	Payload string `json:"payload"` // the script's JSON args, as a string (double-encoded)
}

// egressError matches crates/runlet-wire/src/egress.rs. Owner is "Caller" | "Developer" | "Operator".
type egressError struct {
	Code      string `json:"code"`
	Message   string `json:"message"`
	Source    string `json:"source"`
	Details   any    `json:"details"`
	Retryable bool   `json:"retryable"`
	Owner     string `json:"owner"`
}

// backendMetrics answers Drain; empty slices (not null — serde Vec rejects null).
type backendMetrics struct {
	Db    []any `json:"db"`
	Mongo []any `json:"mongo"`
	Mail  []any `json:"mail"`
	Redis []any `json:"redis"`
	Amq   []any `json:"amq"`
	Auth  []any `json:"auth"`
}

func emptyMetrics() backendMetrics {
	return backendMetrics{[]any{}, []any{}, []any{}, []any{}, []any{}, []any{}}
}

// ---- framing --------------------------------------------------------------------------------

func readFrame(r io.Reader) ([]byte, error) {
	var lenBuf [4]byte
	if _, err := io.ReadFull(r, lenBuf[:]); err != nil {
		return nil, err // io.EOF at a boundary => clean session end
	}
	n := binary.LittleEndian.Uint32(lenBuf[:])
	if n > maxFrame {
		return nil, fmt.Errorf("frame exceeds size cap: %d", n)
	}
	buf := make([]byte, n)
	if _, err := io.ReadFull(r, buf); err != nil {
		return nil, err
	}
	return buf, nil
}

func writeFrame(w io.Writer, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	if len(b) > maxFrame {
		return fmt.Errorf("frame too large to encode")
	}
	var lenBuf [4]byte
	binary.LittleEndian.PutUint32(lenBuf[:], uint32(len(b)))
	if _, err := w.Write(lenBuf[:]); err != nil {
		return err
	}
	_, err = w.Write(b)
	return err
}

// decodeRequest splits an externally-tagged WireRequest frame into (variant, body).
// Unit variants ("Drain") arrive as a bare JSON string; the rest as a one-key object.
func decodeRequest(frame []byte) (variant string, body json.RawMessage, err error) {
	t := bytes.TrimSpace(frame)
	if len(t) == 0 {
		return "", nil, errors.New("empty frame")
	}
	if t[0] == '"' {
		var s string
		if err := json.Unmarshal(frame, &s); err != nil {
			return "", nil, err
		}
		return s, nil, nil
	}
	var m map[string]json.RawMessage
	if err := json.Unmarshal(frame, &m); err != nil {
		return "", nil, err
	}
	for k, v := range m {
		return k, v, nil
	}
	return "", nil, errors.New("empty request object")
}

// Response constructors producing the exact externally-tagged JSON the box deserializes.
func ack() any { return "Ack" }
func initError(code, msg string) any {
	return map[string]any{"InitError": map[string]string{"code": code, "message": msg}}
}
func replyOk(jsonStr string) any   { return map[string]any{"Reply": map[string]any{"Ok": jsonStr}} }
func replyErr(e egressError) any   { return map[string]any{"Reply": map[string]any{"Err": e}} }
func metrics(m backendMetrics) any { return map[string]any{"Metrics": m} }
func protocolError(msg string) any { return map[string]any{"ProtocolError": msg} }

// ---- backends: resolve a logical name to a handler. Swap these for real drivers. ----------

type backend func(action string, payload map[string]any, tenant, actor string) (string, *egressError)

var (
	mu    sync.Mutex
	store = map[string]any{} // key: tenant + "/" + key
)

func cacheBackend(action string, p map[string]any, tenant, _ string) (string, *egressError) {
	mu.Lock()
	defer mu.Unlock()
	key := tenant + "/" + fmt.Sprint(p["key"])
	switch action {
	case "set":
		store[key] = p["value"]
		return `{"ok":true}`, nil
	case "get":
		return marshal(map[string]any{"value": store[key]}), nil
	default:
		return "", &egressError{Code: "UNKNOWN_ACTION", Message: "no such cache action: " + action,
			Source: "cache", Retryable: false, Owner: "Developer"}
	}
}

func ordersBackend(action string, p map[string]any, tenant, actor string) (string, *egressError) {
	switch action {
	case "insert":
		p["by"] = actor
		p["tenant"] = tenant
		return marshal(p), nil
	default:
		return "", &egressError{Code: "UNKNOWN_ACTION", Message: "no such orders action: " + action,
			Source: "orders", Retryable: false, Owner: "Developer"}
	}
}

var backends = map[string]backend{"cache": cacheBackend, "orders": ordersBackend}

func marshal(v any) string { b, _ := json.Marshal(v); return string(b) }

// ---- session handling ----------------------------------------------------------------------

func handleStream(stream quic.Stream, token string) {
	defer stream.Close()

	// 1. Init handshake.
	frame, err := readFrame(stream)
	if err != nil {
		return
	}
	variant, body, err := decodeRequest(frame)
	if err != nil || variant != "Init" {
		_ = writeFrame(stream, protocolError("expected Init first"))
		return
	}
	var init wireInit
	if err := json.Unmarshal(body, &init); err != nil {
		_ = writeFrame(stream, protocolError("bad Init: "+err.Error()))
		return
	}
	if token != "" && (init.Token == nil || *init.Token != token) {
		_ = writeFrame(stream, initError("UNAUTHENTICATED", "invalid or missing box auth token"))
		return
	}
	for _, name := range init.Resources {
		if _, ok := backends[name]; !ok {
			_ = writeFrame(stream, initError("RESOURCE_NOT_FOUND", "no binding for name: "+name))
			return
		}
	}
	tenant, actor := deref(init.Tenant), deref(init.Actor)
	log.Printf("session open: resources=%v tenant=%q actor=%q", init.Resources, tenant, actor)
	if err := writeFrame(stream, ack()); err != nil {
		return
	}

	// 2. Calls, then Drain.
	for {
		frame, err := readFrame(stream)
		if err != nil {
			return // EOF => box closed the session
		}
		variant, body, err := decodeRequest(frame)
		if err != nil {
			_ = writeFrame(stream, protocolError("bad frame: "+err.Error()))
			return
		}
		switch variant {
		case "Call":
			var call wireCall
			if err := json.Unmarshal(body, &call); err != nil {
				_ = writeFrame(stream, protocolError("bad Call"))
				return
			}
			_ = writeFrame(stream, dispatch(call, tenant, actor))
		case "Drain":
			_ = writeFrame(stream, metrics(emptyMetrics()))
		default:
			_ = writeFrame(stream, protocolError("unexpected frame: "+variant))
			return
		}
	}
}

func dispatch(call wireCall, tenant, actor string) any {
	be, ok := backends[call.Name]
	if !ok {
		return replyErr(egressError{Code: "RESOURCE_NOT_FOUND", Message: "no binding for " + call.Name,
			Source: call.Name, Retryable: false, Owner: "Operator"})
	}
	payload := map[string]any{} // non-nil: backends may write into it
	if call.Payload != "" && call.Payload != "null" {
		if err := json.Unmarshal([]byte(call.Payload), &payload); err != nil {
			return replyErr(egressError{Code: "BAD_PAYLOAD", Message: err.Error(),
				Source: call.Name, Retryable: false, Owner: "Developer"})
		}
	}
	out, eerr := be(call.Action, payload, tenant, actor)
	if eerr != nil {
		return replyErr(*eerr)
	}
	return replyOk(out)
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

// ---- TLS: self-signed cert + printed pin ---------------------------------------------------

func selfSignedCert(serverName string) (tls.Certificate, string) {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		log.Fatalf("keygen: %v", err)
	}
	tmpl := x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: serverName},
		DNSNames:              []string{serverName},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(10 * 365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
	}
	der, err := x509.CreateCertificate(rand.Reader, &tmpl, &tmpl, &key.PublicKey, key)
	if err != nil {
		log.Fatalf("cert: %v", err)
	}
	sum := sha256.Sum256(der) // the box pins SHA-256 of the DER-encoded leaf cert
	cert := tls.Certificate{
		Certificate: [][]byte{der},
		PrivateKey:  key,
	}
	return cert, hex.EncodeToString(sum[:])
}

func main() {
	addr := getenv("BROKER_ADDR", defaultAddr)
	serverName := getenv("BROKER_SERVER_NAME", "runlet-broker")
	token := getenv("BROKER_TOKEN", "dev-secret-token")

	cert, pin := selfSignedCert(serverName)
	tlsConf := &tls.Config{
		Certificates: []tls.Certificate{cert},
		NextProtos:   []string{alpn},
		MinVersion:   tls.VersionTLS13,
	}
	quicConf := &quic.Config{
		MaxIncomingStreams: 256,
		MaxIdleTimeout:     30 * time.Second,
		KeepAlivePeriod:    10 * time.Second,
	}

	ln, err := quic.ListenAddr(addr, tlsConf, quicConf)
	if err != nil {
		log.Fatalf("listen: %v", err)
	}
	fmt.Printf(`
runlet Go QUIC broker listening on %s
  ALPN               : %s
  server_name        : %s
  server_cert_pin    : %s
  auth_token         : %s

Paste this into the box config (broker_quic):
  "broker_quic": {
    "replicas": ["%s"],
    "server_name": "%s",
    "server_cert_pin": "%s",
    "auth_token": "%s"
  }

`, addr, alpn, serverName, pin, token, addr, serverName, pin, token)

	for {
		conn, err := ln.Accept(context.Background())
		if err != nil {
			log.Printf("accept: %v", err)
			continue
		}
		go serveConn(conn, token)
	}
}

func serveConn(conn quic.Connection, token string) {
	for {
		stream, err := conn.AcceptStream(context.Background())
		if err != nil {
			return // connection closed
		}
		go handleStream(stream, token)
	}
}

func getenv(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}
