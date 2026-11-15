use std::io;
use std::os::fd::OwnedFd;

use nix::sys::termios::{
    self, BaudRate, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg,
};
use russh::Pty;

pub(super) fn apply_modes(fd: &OwnedFd, modes: &[(Pty, u32)]) -> io::Result<()> {
    let mut settings = termios::tcgetattr(fd).map_err(io::Error::from)?;
    for &(mode, value) in modes {
        let character = match mode {
            Pty::VINTR => Some(libc::VINTR),
            Pty::VQUIT => Some(libc::VQUIT),
            Pty::VERASE => Some(libc::VERASE),
            Pty::VKILL => Some(libc::VKILL),
            Pty::VEOF => Some(libc::VEOF),
            Pty::VEOL => Some(libc::VEOL),
            Pty::VEOL2 => Some(libc::VEOL2),
            Pty::VSTART => Some(libc::VSTART),
            Pty::VSTOP => Some(libc::VSTOP),
            Pty::VSUSP => Some(libc::VSUSP),
            Pty::VREPRINT => Some(libc::VREPRINT),
            Pty::VWERASE => Some(libc::VWERASE),
            Pty::VLNEXT => Some(libc::VLNEXT),
            Pty::VFLUSH | Pty::VDISCARD => Some(libc::VDISCARD),
            #[cfg(target_os = "linux")]
            Pty::VSWTCH => Some(libc::VSWTC),
            #[cfg(target_os = "macos")]
            Pty::VDSUSP => Some(libc::VDSUSP),
            #[cfg(target_os = "macos")]
            Pty::VSTATUS => Some(libc::VSTATUS),
            _ => None,
        };
        if let Some(index) = character {
            if let Ok(value) = u8::try_from(value) {
                settings.control_chars[index] = if value == 255 {
                    libc::_POSIX_VDISABLE
                } else {
                    value
                };
            }
            continue;
        }
        macro_rules! flag {
            ($field:ident, $kind:ident, $flag:ident) => {
                settings.$field.set($kind::$flag, value != 0)
            };
        }
        match mode {
            Pty::IGNPAR => flag!(input_flags, InputFlags, IGNPAR),
            Pty::PARMRK => flag!(input_flags, InputFlags, PARMRK),
            Pty::INPCK => flag!(input_flags, InputFlags, INPCK),
            Pty::ISTRIP => flag!(input_flags, InputFlags, ISTRIP),
            Pty::INLCR => flag!(input_flags, InputFlags, INLCR),
            Pty::IGNCR => flag!(input_flags, InputFlags, IGNCR),
            Pty::ICRNL => flag!(input_flags, InputFlags, ICRNL),
            #[cfg(target_os = "linux")]
            Pty::IUCLC => flag!(input_flags, InputFlags, IUCLC),
            Pty::IXON => flag!(input_flags, InputFlags, IXON),
            Pty::IXANY => flag!(input_flags, InputFlags, IXANY),
            Pty::IXOFF => flag!(input_flags, InputFlags, IXOFF),
            Pty::IMAXBEL => flag!(input_flags, InputFlags, IMAXBEL),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Pty::IUTF8 => flag!(input_flags, InputFlags, IUTF8),
            Pty::ISIG => flag!(local_flags, LocalFlags, ISIG),
            Pty::ICANON => flag!(local_flags, LocalFlags, ICANON),
            #[cfg(target_os = "linux")]
            Pty::XCASE => flag!(local_flags, LocalFlags, XCASE),
            Pty::ECHO => flag!(local_flags, LocalFlags, ECHO),
            Pty::ECHOE => flag!(local_flags, LocalFlags, ECHOE),
            Pty::ECHOK => flag!(local_flags, LocalFlags, ECHOK),
            Pty::ECHONL => flag!(local_flags, LocalFlags, ECHONL),
            Pty::NOFLSH => flag!(local_flags, LocalFlags, NOFLSH),
            Pty::TOSTOP => flag!(local_flags, LocalFlags, TOSTOP),
            Pty::IEXTEN => flag!(local_flags, LocalFlags, IEXTEN),
            Pty::ECHOCTL => flag!(local_flags, LocalFlags, ECHOCTL),
            Pty::ECHOKE => flag!(local_flags, LocalFlags, ECHOKE),
            Pty::PENDIN => flag!(local_flags, LocalFlags, PENDIN),
            Pty::OPOST => flag!(output_flags, OutputFlags, OPOST),
            #[cfg(target_os = "linux")]
            Pty::OLCUC => flag!(output_flags, OutputFlags, OLCUC),
            Pty::ONLCR => flag!(output_flags, OutputFlags, ONLCR),
            Pty::OCRNL => flag!(output_flags, OutputFlags, OCRNL),
            Pty::ONOCR => flag!(output_flags, OutputFlags, ONOCR),
            Pty::ONLRET => flag!(output_flags, OutputFlags, ONLRET),
            Pty::CS7 | Pty::CS8 if value != 0 => {
                settings.control_flags.remove(ControlFlags::CSIZE);
                settings.control_flags.insert(if mode == Pty::CS7 {
                    ControlFlags::CS7
                } else {
                    ControlFlags::CS8
                });
            }
            Pty::PARENB => flag!(control_flags, ControlFlags, PARENB),
            Pty::PARODD => flag!(control_flags, ControlFlags, PARODD),
            Pty::TTY_OP_ISPEED | Pty::TTY_OP_OSPEED => {
                if let Some(speed) = baud_rate(value) {
                    if mode == Pty::TTY_OP_ISPEED {
                        termios::cfsetispeed(&mut settings, speed).map_err(io::Error::from)?;
                    } else {
                        termios::cfsetospeed(&mut settings, speed).map_err(io::Error::from)?;
                    }
                }
            }
            _ => {}
        }
    }
    termios::tcsetattr(fd, SetArg::TCSANOW, &settings).map_err(io::Error::from)
}

fn baud_rate(speed: u32) -> Option<BaudRate> {
    Some(match speed {
        0 => BaudRate::B0,
        50 => BaudRate::B50,
        75 => BaudRate::B75,
        110 => BaudRate::B110,
        134 => BaudRate::B134,
        150 => BaudRate::B150,
        200 => BaudRate::B200,
        300 => BaudRate::B300,
        600 => BaudRate::B600,
        1200 => BaudRate::B1200,
        1800 => BaudRate::B1800,
        2400 => BaudRate::B2400,
        4800 => BaudRate::B4800,
        9600 => BaudRate::B9600,
        19200 => BaudRate::B19200,
        38400 => BaudRate::B38400,
        57600 => BaudRate::B57600,
        115200 => BaudRate::B115200,
        230400 => BaudRate::B230400,
        #[cfg(target_os = "linux")]
        460800 => BaudRate::B460800,
        #[cfg(target_os = "linux")]
        921600 => BaudRate::B921600,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_modes_reach_the_terminal_and_disabled_characters_are_portable() {
        let pty = nix::pty::openpty(None, None).expect("open test PTY");
        apply_modes(
            &pty.slave,
            &[
                (Pty::ECHO, 0),
                (Pty::ICANON, 0),
                (Pty::IXON, 0),
                (Pty::OPOST, 0),
                (Pty::VINTR, 26),
                (Pty::VEOF, 255),
                (Pty::TTY_OP_ISPEED, 9600),
                (Pty::TTY_OP_OSPEED, 9600),
            ],
        )
        .expect("apply requested modes");
        let settings = termios::tcgetattr(&pty.slave).expect("inspect actual terminal");
        assert!(
            !settings
                .local_flags
                .intersects(LocalFlags::ECHO | LocalFlags::ICANON)
        );
        assert!(!settings.input_flags.contains(InputFlags::IXON));
        assert!(!settings.output_flags.contains(OutputFlags::OPOST));
        assert_eq!(settings.control_chars[libc::VINTR], 26);
        assert_eq!(settings.control_chars[libc::VEOF], libc::_POSIX_VDISABLE);
        #[cfg(target_os = "macos")]
        let expected_speed = 9600;
        #[cfg(not(target_os = "macos"))]
        let expected_speed = BaudRate::B9600;
        assert_eq!(termios::cfgetispeed(&settings), expected_speed);
        assert_eq!(termios::cfgetospeed(&settings), expected_speed);
    }
}
