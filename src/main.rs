use anyhow::Result;
use clap::Parser;
use nix::pty::Winsize;
use nix::sys::select::FdSet;
use nix::sys::termios::{self, InputFlags, LocalFlags, OutputFlags, Termios};
use nix::sys::time::TimeVal;
use nix::sys::wait::WaitStatus;
use nix::unistd::{ForkResult, Pid};
use signal_hook::{consts::SIGWINCH, iterator::Signals};
use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::io::AsRawFd;
use std::thread;

/// A mapping from input byte sequences to output byte sequences for key remapping.
type KeyMap = HashMap<Vec<u8>, Vec<u8>>;

/// Command line arguments for the CLI key hook program.
#[derive(Parser)]
#[command(name = "cli-keyhook")]
#[command(version)]
#[command(about = "A CLI wrapper that intercepts and remaps keyboard input")]
struct Args {
    /// Map input bytes to output bytes (hex format)
    #[arg(short = 'k', long = "keymap", value_name = "INPUT:OUTPUT", value_parser = parse_keymap)]
    keymaps: Vec<(Vec<u8>, Vec<u8>)>,

    /// Command to execute
    command: String,

    /// Arguments for the command
    args: Vec<String>,
}

/// Main entry point for the CLI key hook program.
///
/// Parses command line arguments, sets up key mappings, and runs the PTY wrapper.
fn main() -> Result<()> {
    let args = Args::parse();

    run_pty_wrapper(&args.command, &args.args, KeyMap::from_iter(args.keymaps))
}

/// Parses a keymap string in the format "input_hex:output_hex".
///
/// # Arguments
/// * `s` - A string in the format "input_hex:output_hex"
///
/// # Returns
/// * `Ok((input_bytes, output_bytes))` on success
/// * `Err(error_message)` on parsing failure
fn parse_keymap(s: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(format!(
            "invalid keymap format '{s}', expected format 'input_hex:output_hex'"
        ));
    }

    let input_bytes = hex_decode(parts[0])
        .map_err(|e| format!("invalid input hex string '{}' ({})", parts[0], e))?;

    let output_bytes = if parts[1].is_empty() {
        Vec::new()
    } else {
        hex_decode(parts[1])
            .map_err(|e| format!("invalid output hex string '{}' ({})", parts[1], e))?
    };

    Ok((input_bytes, output_bytes))
}

/// Decodes a hexadecimal string into a vector of bytes.
///
/// # Arguments
/// * `hex_str` - A hexadecimal string with even length
///
/// # Returns
/// * `Ok(bytes)` on successful decoding
/// * `Err(error_message)` on invalid hex format
fn hex_decode(hex_str: &str) -> Result<Vec<u8>, String> {
    if hex_str.is_empty() {
        return Err("hex string cannot be empty".into());
    }

    if !hex_str.len().is_multiple_of(2) {
        return Err(format!(
            "hex string must have even length, got {} characters",
            hex_str.len()
        ));
    }

    let mut bytes = Vec::new();
    for i in (0..hex_str.len()).step_by(2) {
        let byte_str = &hex_str[i..i + 2];
        let byte = u8::from_str_radix(byte_str, 16)
            .map_err(|_| format!("invalid hex characters '{byte_str}' at position {i}"))?;
        bytes.push(byte);
    }

    Ok(bytes)
}

/// Runs the main PTY wrapper that forks into parent and child processes.
///
/// # Arguments
/// * `command` - The command to execute in the child process
/// * `args` - Arguments for the command
/// * `keymap` - Key mapping configuration for input transformation
fn run_pty_wrapper(command: &str, args: &[String], keymap: KeyMap) -> Result<()> {
    let winsize = get_terminal_size()?;
    let pty = nix::pty::openpty(&winsize, None)?;

    let master = pty.master;
    let slave = pty.slave;

    let original_termios = save_terminal_settings()?;

    // SAFETY: only `close` and `dup2` are called before child's `execvp`.
    match unsafe { nix::unistd::fork() }? {
        ForkResult::Parent { child } => {
            drop(slave); // Close slave fd

            setup_raw_mode()?;
            setup_signal_handler(&master)?;

            let result = parent_process(master, child, keymap);

            restore_terminal_settings(&original_termios)?;

            result
        }
        ForkResult::Child => {
            drop(master); // Close master fd
            child_process(slave, command, args)
        }
    }
}

/// Handles the parent process logic for PTY communication.
///
/// Manages input/output between stdin/stdout and the PTY master,
/// applying key mappings to user input.
///
/// # Arguments
/// * `master` - PTY master file descriptor
/// * `child_pid` - Process ID of the child process
/// * `keymap` - Key mapping configuration
fn parent_process(master: OwnedFd, child_pid: Pid, keymap: KeyMap) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();

    let mut buffer = [0u8; 16384];
    let mut child_exited = false;

    loop {
        let mut read_fds = FdSet::new();
        read_fds.insert(stdin.as_fd());
        read_fds.insert(master.as_fd());

        let mut timeout = TimeVal::new(0, 100_000); // 100ms

        match nix::sys::select::select(
            Some(std::cmp::max(stdin.as_raw_fd(), master.as_raw_fd()) + 1),
            Some(&mut read_fds),
            None,
            None,
            Some(&mut timeout),
        ) {
            Ok(n) => {
                // Check child process status on every iteration
                if let Ok(status) =
                    nix::sys::wait::waitpid(child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
                    && status != WaitStatus::StillAlive
                {
                    child_exited = true;
                    break;
                }

                if n != 0 {
                    if read_fds.contains(stdin.as_fd()) {
                        match nix::unistd::read(&stdin, &mut buffer) {
                            Ok(0) => break,
                            Ok(n) => {
                                let processed_input = process_input_hook(&buffer[..n], &keymap);
                                nix::unistd::write(&master, &processed_input)?;
                            }
                            Err(_) => continue,
                        }
                    }

                    if read_fds.contains(master.as_fd()) {
                        match nix::unistd::read(&master, &mut buffer) {
                            Ok(0) => break,
                            Ok(n) => {
                                nix::unistd::write(&stdout, &buffer[..n])?;
                            }
                            Err(_) => continue,
                        }
                    }
                }
            }
            Err(_) => continue,
        }
    }

    // Only call waitpid if child process hasn't exited yet
    if !child_exited {
        nix::sys::wait::waitpid(child_pid, None)?;
    }
    drop(master); // Explicitly close master fd

    Ok(())
}

/// Handles the child process logic for command execution.
///
/// Redirects stdin/stdout/stderr to the PTY slave and executes the specified command.
///
/// # Arguments
/// * `slave` - PTY slave file descriptor
/// * `command` - Command to execute
/// * `args` - Arguments for the command
fn child_process(slave: OwnedFd, command: &str, args: &[String]) -> Result<()> {
    nix::unistd::dup2_stdin(&slave)?;
    nix::unistd::dup2_stdout(&slave)?;
    nix::unistd::dup2_stderr(&slave)?;

    drop(slave); // Explicitly close slave fd

    let cmd = CString::new(command)?;
    let mut exec_args: Vec<CString> = vec![cmd.clone()];
    for arg in args {
        exec_args.push(CString::new(arg.as_str())?);
    }

    nix::unistd::execvp(&cmd, &exec_args)?;

    Ok(())
}

/// Processes input bytes by applying key mappings.
///
/// Scans the input for byte sequences that match keymap entries
/// and replaces them with their corresponding output sequences.
///
/// # Arguments
/// * `input` - Input byte sequence from user
/// * `keymap` - Key mapping configuration
///
/// # Returns
/// Processed byte sequence with mappings applied
fn process_input_hook(input: &[u8], keymap: &KeyMap) -> Vec<u8> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < input.len() {
        let mut matched = false;

        // Process all substring matches (naive implementation)
        for (hook_key, mapped) in keymap {
            if input[i..].starts_with(hook_key) {
                // If hook is found, add corresponding output
                result.extend_from_slice(mapped);
                i += hook_key.len();
                matched = true;
                break;
            }
        }

        // If no hook matched, send the byte as-is
        if !matched {
            result.push(input[i]);
            i += 1;
        }
    }

    result
}

/// Saves the current terminal settings.
///
/// # Returns
/// Current terminal configuration for later restoration
fn save_terminal_settings() -> Result<Termios, nix::Error> {
    termios::tcgetattr(io::stdin())
}

/// Restores terminal settings to a previous state.
///
/// # Arguments
/// * `termios` - Terminal configuration to restore
fn restore_terminal_settings(termios: &Termios) -> Result<(), nix::Error> {
    termios::tcsetattr(io::stdin(), termios::SetArg::TCSANOW, termios)
}

/// Sets up raw mode for terminal input.
///
/// Disables canonical mode, echo, and signal processing to allow
/// direct character-by-character input handling.
fn setup_raw_mode() -> Result<(), nix::Error> {
    let stdin = io::stdin();
    let mut termios = termios::tcgetattr(&stdin)?;

    termios.input_flags &= !(InputFlags::ICRNL | InputFlags::IXON);
    termios.local_flags &= !(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
    termios.output_flags &= !OutputFlags::OPOST;

    termios.control_chars[termios::SpecialCharacterIndices::VMIN as usize] = 1;
    termios.control_chars[termios::SpecialCharacterIndices::VTIME as usize] = 0;

    termios::tcsetattr(&stdin, termios::SetArg::TCSANOW, &termios)
}

/// Gets the current terminal window size.
///
/// # Returns
/// Window size structure with rows, columns, and pixel dimensions
fn get_terminal_size() -> Result<Winsize, nix::Error> {
    let mut winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        nix::libc::ioctl(
            io::stdout().as_raw_fd(),
            nix::libc::TIOCGWINSZ,
            &mut winsize,
        );
    }
    Ok(winsize)
}

/// Sets up signal handling for window resize events.
///
/// Spawns a background thread to handle SIGWINCH signals and
/// forward window size changes to the PTY.
///
/// # Arguments
/// * `master` - PTY master file descriptor for ioctl calls
fn setup_signal_handler(master: &OwnedFd) -> Result<()> {
    let master_fd = master.as_raw_fd(); // Get raw fd for use in signal handler

    thread::spawn(move || {
        let mut signals = match Signals::new([SIGWINCH]) {
            Ok(s) => s,
            Err(_) => return,
        };

        for signal in signals.forever() {
            if signal == SIGWINCH
                && let Ok(winsize) = get_terminal_size()
            {
                unsafe {
                    nix::libc::ioctl(master_fd, nix::libc::TIOCSWINSZ, &winsize);
                }
            }
        }
    });

    Ok(())
}
