# waybar-codex-usage

Minimal [Waybar](https://github.com/Alexays/Waybar) custom module that shows Codex ChatGPT usage limits using the same credentials as the Codex CLI. 

## Requirements

- Run `codex login` with ChatGPT authentication once.
- `auth.json` stored in `~/.codex` or keyring (based on Codex config).

## Build and Installation

```bash
cargo build --release
```
Put `target/release/waybar-codex-usage` on your PATH (or use the absolute path in your Waybar config).

## Usage

```bash
waybar-codex-usage --compact
waybar-codex-usage --format "C {pct}% ({win})"
waybar-codex-usage --use-weekly
```

Output is Waybar compatible JSON with `text`, `tooltip`, `class`, and `percentage`.

### Formatting tokens

- `{pct}` active window percent
- `{reset}` active window reset ETA
- `{win}` active window label (e.g. `5h`, `7d`)
- `{p_pct}` 5h percent
- `{p_reset}` 5h reset ETA
- `{s_pct}` 7d percent
- `{s_reset}` 7d reset ETA
- `{credits}` credits balance or `Unlimited`

### Flags

- `--compact` short output
- `--format` custom text format
- `--tooltip` custom tooltip format
- `--use-weekly` prefer weekly window as the active display
- `--no-credits` hide credits line in tooltip
- `--cache-ttl` cache lifetime in seconds (default 60, set 0 to disable)

## Waybar configuration

Add the module to your Waybar config (typically `~/.config/waybar/config.jsonc`).

```json
"custom/codex-usage": {
  "exec": "/path/to/waybar-codex-usage",
  "interval": 60,
  "return-type": "json",
  "format": "{}",
  "tooltip": true
}
```

## Notes

- Config is read from `~/.codex/config.toml` (or `CODEX_HOME`).
- `chatgpt_base_url` defaults to `https://chatgpt.com/backend-api` when not set.
- `cli_auth_credentials_store` supports `auto`, `file`, or `keyring`.
