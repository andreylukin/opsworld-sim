# opsworld-sim

![A terminal recording of seed 10](docs/demo.gif)

opsworld-sim tests a logistics ops agent against injected faults in a simulated ERP and TMS. A seed fixes the world and the fault schedule, so I can replay any failure exactly.

Seed 10 is the one to look at first. A receipt write times out after it has already committed, and the naive `v1` policy retries it. The receipt applies twice and on_hand goes 50 -> 210 -> 370. The `v2` policy reads state after the timeout, sees the receipt landed, and does not write again.

You can run that case with a stable Rust toolchain:

```sh
cargo build --release
cargo run -q --release -- explore --runs 500 --policy v1 --stop-on no-double-apply
cargo run -q --release -- replay repros/v1-seed-00010.json
cargo run -q --release -- replay repros/v1-seed-00010.json --policy v2
```

The `explore` command stops at seed 10 and saves the repro file the two replays read. It and the `v1` replay exit 1 because they report a violation.

The agents are scripted by default. An optional LLM mode drives the same tools with a real model and needs a logged-in Claude Code CLI:

```sh
cargo run -q --release -- llm --seeds 8,10,20 --model haiku
```

I checked in one recorded run, which replays with no model or login:

```sh
cargo run -q --release -- replay examples/llm-seed-00010.json
```

The ERP and TMS are in-memory stand-ins, so a pass here does not prove a real system is safe. The full results and the fault and invariant lists are in [docs/DETAILS.md](docs/DETAILS.md).

I built this with [Claude Code](https://claude.com/claude-code). It is under the [MIT](LICENSE) license.
