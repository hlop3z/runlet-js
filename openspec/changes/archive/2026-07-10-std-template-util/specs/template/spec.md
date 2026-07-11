## ADDED Requirements

### Requirement: `$std.template` compiles templates in explicit escaping modes

The system SHALL expose a `$std.template` value-util with exactly two compile entry points that
each take a template source string and return a compiled template object:

- `$std.template.html(source)` SHALL compile the source with **HTML auto-escaping enabled**, so
  interpolated values have `& < > " '` replaced with their HTML entities.
- `$std.template.text(source)` SHALL compile the source with **no escaping**, emitting interpolated
  values verbatim.

There SHALL be no ambiguous-default single-argument compile call: an author always states `html`
or `text`. Template syntax SHALL be Jinja2 (`{{ expr }}` expressions, `{% stmt %}` statements).

#### Scenario: HTML mode escapes interpolated values

- **WHEN** a handler runs `$std.template.html("<p>{{ name }}</p>").render({ name: "<b>&x" })`
- **THEN** the result is `<p>&lt;b&gt;&amp;x</p>` (the interpolated value is entity-escaped, the
  literal markup in the template is not)

#### Scenario: Text mode emits values verbatim

- **WHEN** a handler runs `$std.template.text("Hi {{ name }}").render({ name: "<b>&x" })`
- **THEN** the result is `Hi <b>&x` (no escaping applied)

#### Scenario: Statements and expressions render

- **WHEN** a handler renders `$std.template.text("{% for i in items %}{{ i }},{% endfor %}")` with
  context `{ items: [1, 2, 3] }`
- **THEN** the result is `1,2,3,`

### Requirement: Compiled templates render a JSON context to a string

A compiled template object SHALL expose a `.render(context)` method that takes a plain JSON object
and returns the rendered string. The context object SHALL be the sole source of variable values;
the render SHALL NOT read any host, request, or ambient state.

#### Scenario: Nested context access

- **WHEN** a handler renders `$std.template.text("{{ user.name }} owes {{ amount }}")` with
  `{ user: { name: "Ada" }, amount: "10.00" }`
- **THEN** the result is `Ada owes 10.00`

#### Scenario: Render is reusable across contexts

- **WHEN** a handler compiles a template once and calls `.render(a)` then `.render(b)` with two
  different contexts
- **THEN** each call returns the string for its own context, independently

### Requirement: Undefined variables are lenient with a settable placeholder

A compiled template SHALL render an undefined or missing variable as the **empty string** by
default (rendering never fails on an absent merge tag). The compiled template SHALL expose a
`.missing(placeholder)` method returning a template whose undefined variables render as the given
placeholder string instead.

#### Scenario: Missing variable renders empty by default

- **WHEN** a handler renders `$std.template.text("A{{ gap }}B").render({})`
- **THEN** the result is `AB` (no error, the missing variable is empty)

#### Scenario: Placeholder substitutes for missing variables

- **WHEN** a handler renders `$std.template.text("A{{ gap }}B").missing("—").render({})`
- **THEN** the result is `A—B`

### Requirement: Templates expose their referenced merge tags

A compiled template SHALL expose a `.fields()` method returning the list of top-level variable
names the template references (its merge tags), so a caller can determine what data a template
needs before rendering it.

#### Scenario: Fields lists referenced variables

- **WHEN** a handler calls `$std.template.text("{{ first }} {{ last }} — {{ first }}").fields()`
- **THEN** the result contains `first` and `last` (each referenced name once, order-independent)

#### Scenario: Fields is empty for a static template

- **WHEN** a handler calls `$std.template.text("no variables here").fields()`
- **THEN** the result is an empty list

### Requirement: `$std.template` is deterministic and available under both profiles

The `$std.template` environment SHALL be constructed with no clock or randomness builtins, so
`render` is a pure function of `(source, context)`. `$std.template` SHALL therefore be present and
identical under both `Profile::Full` and `Profile::Deterministic`, and SHALL NOT be pruned by the
determinism sanitizer.

#### Scenario: Available and pure under Deterministic profile

- **WHEN** a handler runs under `Profile::Deterministic` and renders the same `(source, context)`
  twice
- **THEN** `$std.template.html`/`$std.template.text` are defined and both renders produce the
  identical string

### Requirement: Malformed templates report a capability error, never panic

Compiling a syntactically invalid template SHALL surface a capability error to the script (a thrown
Error identifying the template problem), and SHALL NOT panic the runtime or abort the request
outside the normal error envelope. Rendering SHALL likewise be panic-free.

#### Scenario: Syntax error is a catchable Error

- **WHEN** a handler runs `$std.template.text("{{ unclosed ")` (malformed source)
- **THEN** a JavaScript Error is thrown that the handler can `try/catch`, and the runtime does not
  crash
