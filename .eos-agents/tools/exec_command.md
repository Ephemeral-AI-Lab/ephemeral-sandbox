---
intent: write_allowed
terminal: false
hooks: [destructive_git_shell, destructive_shell]
---
Run a command in a managed PTY session inside the sandbox. If the command finishes within `yield_time_ms` you get the final result; otherwise the session keeps running in the background and you get `status: running` with a `command_session_id`. Use `write_stdin` to feed input or send exact Ctrl-C/Ctrl-D teardown, and use `read_command_progress` to inspect later output without writing stdin. Set `timeout` (seconds) to override the daemon-configured default run bound. Output is a merged PTY stream: everything (including the program's stderr) arrives in `stdout`, and the `stderr` field is always empty.
