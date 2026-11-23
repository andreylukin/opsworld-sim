# Terminal recording of the seed 10 failure and replay.
# From the repo root, after `cargo build --release`:
#   env -u NO_COLOR FORCE_COLOR=1 TERM=xterm-256color vhs docs/demo.tape

Output docs/demo.gif

Require cargo

Set Shell bash
Set FontFamily "DejaVu Sans Mono"
Set FontSize 16
Set Width 1400
Set Height 700
Set Padding 20
Set Theme "Catppuccin Mocha"
Set WindowBar Colorful
Set WindowBarSize 36
Set Framerate 12
Set PlaybackSpeed 1
Set TypingSpeed 20ms

Hide
Type `unset NO_COLOR; export TERM=xterm-256color COLORTERM=truecolor FORCE_COLOR=1 CLICOLOR_FORCE=1`
Enter
Type `PS1='$ '; unset PROMPT_COMMAND`
Enter
Ctrl+L
Show

Sleep 400ms
Type "rm -rf repros"
Enter
Sleep 500ms
Type "cargo run -q --release -- explore --runs 500 --policy v1 --stop-on no-double-apply"
Enter
Sleep 7s
Ctrl+L
Sleep 250ms
Type "cargo run -q --release -- replay repros/v1-seed-00010.json"
Enter
Sleep 8s
Ctrl+L
Sleep 250ms
Type "cargo run -q --release -- replay repros/v1-seed-00010.json --policy v2"
Enter
Sleep 7s
Ctrl+L
Sleep 250ms
Type "cargo run -q --release -- replay examples/llm-seed-00010.json"
Enter
Sleep 6s
