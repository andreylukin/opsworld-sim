# opsworld-sim

A seeded simulation that tests a logistics ops agent against injected faults, then replays any failure exactly.

![A terminal recording of seed 10](docs/demo.gif)

## What it does

Each seed builds a fake ops world of purchase orders, shipments and dock doors, injects a short fault schedule, and checks a few invariants after every step.

When a check fails, the run is saved and can be replayed exactly, and the fault list is shrunk to the minimum that still causes the failure.

On seed 10, a receipt write times out after it has already committed, and a naive retry applies it twice: on_hand goes from 50 to 210 to 370. The careful policy reads state first and does not write again.

Results, the fault and invariant lists, and the optional LLM mode are in [docs/DETAILS.md](docs/DETAILS.md).

## Run it

Needs a stable Rust toolchain.

```sh
cargo build --release
cargo run -q --release -- explore --runs 500 --policy v1 --stop-on no-double-apply
cargo run -q --release -- replay repros/v1-seed-00010.json
cargo run -q --release -- replay repros/v1-seed-00010.json --policy v2
```

Optional LLM mode: `cargo run -q --release -- llm --seeds 8,10,20 --model haiku`. A checked-in recording at `examples/llm-seed-00010.json` can be replayed with no model or login.

## What it is not

- The ERP and TMS are in-memory stand-ins. A pass here does not prove a real system is safe.
- The default agents are scripted policies. An optional LLM mode can drive the same tools, but it is not required for build or tests.
- This is a sketch, not a product.

Built with [Claude Code](https://claude.com/claude-code). [MIT](LICENSE) license.
