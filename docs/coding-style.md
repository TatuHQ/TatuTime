# Coding style

The Rust style bar for TatuTime is **idiomatic, example-led Rust** — closer to
[*Rust by Example*](https://doc.rust-lang.org/rust-by-example/) than to a formal
specification. That book is the reference exemplar: short, purposeful modules,
a one-line statement of what each item does, and a tiny example when the shape of
a call is not obvious from the signature.

The shared rules under [`AgentRules/`](../AgentRules/) still apply; this page
records only the documentation bar on top of them.

## Documentation

**Module headers** open the file with `//!` and stay at **≤ 10 lines**: one line
of purpose, the non-obvious *why*, and at most one short fenced `rust` example.

**Item docs** use `///` and stay at **≤ 4 lines** before an optional short
example. One sentence says *what*; a second only when the *why* is non-obvious.

**Examples are illustrative, not doctests.** This crate has no doctest harness, so
examples are fenced ` ```rust ` inside the doc comment and kept to a few lines.
They show the shape of a call, not a program that must compile as a test — never
add an example that only passes by compiling.

**Comments state the present contract.** Narration of history is banned: no
"used to", "previously", "no longer", "after the #… split" or "until #…". A past
fix is restated as the guarantee it enforces today, keeping its ticket tag (for
example `(#255)`).

**Ticket tags are traceability, not narrative.** Keep every `(#NNN)` reference —
`(#8)`, `(#114)`, `(#255)` and friends are how a reader finds the change a
decision belongs to.

**`legacy` comments are sacred.** Every comment documenting on-disk legacy or
load-compatibility behaviour survives; reword only for clarity, never delete.

Inline `//` comments may stay where they explain *why*, capped at about two lines.
