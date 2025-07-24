# cli-keyhook

A CLI wrapper that intercepts and remaps keyboard input using PTY (pseudo-terminal).

## Overview

`cli-keyhook` allows you to remap keyboard input for any command-line program by creating a PTY wrapper that intercepts keystrokes and transforms them according to your configuration. This is useful for customizing key bindings without modifying the target program.

## Features

- Remap any key sequence to another key sequence
- Support for multiple key mappings
- Works with most command-line program
- Preserves terminal features like window resizing
- Hexadecimal key specification for precise control

## Installation

- Rust 1.78.0 or higher (MSRV - Minimum Supported Rust Version)

```bash
git clone https://github.com/s-ylide/cli-keyhook.git
cargo install --path cli-keyhook
```

## Usage

```
cli-keyhook [OPTIONS] <COMMAND> [ARGS]...

Arguments:
  <COMMAND>     Command to execute
  [ARGS]...     Arguments for the command

Options:
  -k, --keymap <INPUT:OUTPUT>    Map input bytes to output bytes (hex format)
  -h, --help                     Print help
  -V, --version                  Print version
```

### Key Mapping Format

Key mappings are specified in hexadecimal format as `INPUT:OUTPUT`:

- `INPUT` - Hexadecimal representation of input bytes
- `OUTPUT` - Hexadecimal representation of output bytes (empty for disabling keys)

## Examples

### Basic Usage

```bash
# Prevent accidental termination during long-running processes
cli-keyhook -k "03:" python -- train_model.py

# Remap ESC to Ctrl+C for easier interruption in interactive programs
cli-keyhook -k "1b:03" mysql -- -u user -p

# Disable Ctrl+D to prevent accidental logout
cli-keyhook -k "04:" bash
```

### Accessibility and Comfort

```bash
# Remap Caps Lock (sent as ESC) to Enter for easier navigation
cli-keyhook -k "1b:0d" less -- large_file.txt

# Swap Ctrl+A/E with Home/End keys for users preferring different navigation
cli-keyhook -k "01:1b5b48" -k "05:1b5b46" nano -- document.txt

# Create custom shortcuts for repetitive commands
cli-keyhook -k "71:7175697420" psql -- database_name  # 'q' becomes 'quit '
```

### Development Workflow

```bash
# Prevent accidental database disconnection during development
alias db-safe='cli-keyhook -k "03:" -k "04:" psql --'
db-safe production_db

# Safe git operations with disabled force-quit
alias git-protected='cli-keyhook -k "03:" git --'
git-protected rebase -i HEAD~5

# Custom key mappings for interactive debugging
cli-keyhook -k "73:73746570" -k "63:636f6e74696e7565" gdb -- ./program
```

### Function-based Wrapper

```bash
# Wrapper for database operations with safety features
db-connect() {
    cli-keyhook -k "03:" -k "04:" -k "1a:" "$@"
}
db-connect mysql -- -h production-server

# Interactive program with custom navigation
interactive-tool() {
    cli-keyhook -k "68:6a" -k "6a:6b" -k "6b:68" "$@"  # h/j/k cycling
}
interactive-tool htop
```

### AI Agent Integration with VSCode Terminal

This project was originally created to improve the experience when using CLI-based AI agents like `claude` and `gemini` from integrated terminals.
Sometimes you may accidentally press the Enter key even when not sending a message. When inputting multiple lines, you may forget to press Shift or Alt for each line break. Some input methods for certain languages require you to press Enter to commit the input method's buffer.
It would be nice if you could use Enter to create a new line and Ctrl+Enter to send, like `slack`.

VSCode's `workbench.action.terminal.sendSequence` command allows sending arbitrary byte sequences to focused terminals via keyboard shortcuts. Combined with `cli-keyhook`, you can implement Ctrl+Enter to send instead of Enter:

```bash
# Setup: Create an alias for AI agents that maps Ctrl+Enter to Enter (0x0d)
claude-code() {
    cli-keyhook -k "5c0d:5c0d" -k "e2808b0d:0d" -k "0d:" claude -- "$@"
}
gemini-code() {
    cli-keyhook -k "5c0d:5c0d" -k "e2808b0d:0d" -k "0d:" gemini -- "$@"
}

# Usage
claude-code -c
gemini-code
```

With adding this to your `keybindings.json`:

```json
[
  {
    "...": "existing config"
  },
  {
    "key": "shift+enter",
    "command": "workbench.action.terminal.sendSequence",
    "args": {
      "text": "\\\r\n"
    },
    "when": "terminalFocus"
  },
  {
    "key": "ctrl+enter",
    "command": "workbench.action.terminal.sendSequence",
    "args": {
      "text": "\u200b\n"
    },
    "when": "terminalFocus"
  }
]
```

## Common Key Codes

| Key    | Hex Code |
| ------ | -------- |
| Ctrl+C | 03       |
| Ctrl+D | 04       |
| Ctrl+Z | 1a       |
| ESC    | 1b       |
| Enter  | 0d       |
| \      | 5c       |

This is a rough correspondence and is not guaranteed to be correct in all environments.
You can find which bytes are sent to terminal by `showkey --ascii`.

## How It Works

The program creates a PTY (pseudo-terminal) and forks into two processes:

- **Parent process**: Handles input/output between the user's terminal and the PTY, applying key mappings to user input
- **Child process**: Executes the target command with its stdin/stdout/stderr connected to the PTY

This approach allows transparent key remapping while preserving all terminal features.
