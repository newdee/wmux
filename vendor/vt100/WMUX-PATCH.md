# vt100 0.16.2 with a wmux patch

Copied verbatim from crates.io (`vt100 = "0.16.2"`, MIT licence, see LICENSE)
and wired in through `[patch.crates-io]` in the workspace `Cargo.toml`.

One change, in `src/row.rs`:

- `Row::resize` clears a wide character whose first half ends up in the new
  last column after a shrink (mirrors what `Row::truncate` already does).
- `Row::clear_wide` bounds-checks the neighbouring cell.

Without it, shrinking a pane (any split or window resize) through a CJK
character and then erasing near the right edge panics with
`index out of bounds: the len is N but the index is N` (`row.rs:89`).
Regression test: `server::pane::tests::shrink_through_wide_char_then_erase_does_not_panic`.

Drop this directory and the `[patch]` entry once the fix is released upstream.
