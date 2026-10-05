# Contributing

This is a harness, not a product. A result that it produces must be one that a person who does not trust us can reproduce.

The house rules for Rust style, prose and commits are in [CONTRIBUTING.md in tamnd/rudb](https://github.com/tamnd/rudb/blob/main/CONTRIBUTING.md). They apply here. `cargo test` checks the prose rules.

## Rules

**The harness does not link rudb.** It reads rudb's bytes with its own code. If you need a decoder, write it here.

**A change to a comparison comes with a case that shows it.** A harness bug that makes a case pass hides an engine bug. If you make a comparison less strict, say what it hid and why the change is correct.

**An exclusion comes with a reason.** A case that the harness does not count is counted in its own column. An expected failure names a rudb issue.

**The oracle is correct.** When the oracle and the PostgreSQL documentation disagree, the oracle wins.

**A pin moves in its own commit.** The commit message gives the change in each number that the move caused.

**Every number has a source.** A result records the rudb commit, the oracle commit, the client commit and the configuration that produced it.

## Running it

```
cargo test
cargo run --release -- oracle build
cargo run --release -- up --twin
cargo run --release -- diff corpus/differential/smoke.sql
cargo run --release -- down
```

## License

Apache-2.0. A contribution is offered under the same terms.
