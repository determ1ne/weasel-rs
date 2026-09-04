use crate::bindings::{GetKeyboardLayout, GetKeyboardState, ToUnicodeEx};
use weasel_common::message::KeyEvent;

const EMOJI_SHORTCUT_TAG: usize = 0x57525345;

pub fn is_emoji_shortcut() -> bool {
    unsafe { crate::bindings::GetMessageExtraInfo().0 as usize == EMOJI_SHORTCUT_TAG }
}

/// Invoked only for the user's emoji action, on the foreground TSF thread.
pub fn open_emoji_panel() {
    use crate::bindings::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_LWIN,
        VK_OEM_PERIOD,
    };
    let inputs = [
        (VK_LWIN, false),
        (VK_OEM_PERIOD, false),
        (VK_OEM_PERIOD, true),
        (VK_LWIN, true),
    ]
    .map(|(key, up)| INPUT {
        r#type: INPUT_KEYBOARD as u32,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key as u16,
                dwFlags: if up { KEYEVENTF_KEYUP as u32 } else { 0 },
                dwExtraInfo: EMOJI_SHORTCUT_TAG,
                ..Default::default()
            },
        },
    });
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!(
                "weasel-tip: emoji shortcut injection incomplete: {sent}/{}",
                inputs.len()
            ),
        );
    }
}

/// Translate on the TSF input thread, before crossing into the RPC worker.
pub fn translate(vk: u32, lparam: i64, key_up: bool) -> KeyEvent {
    let mut event = KeyEvent {
        virtual_key: vk,
        lparam,
        key_up,
        ..Default::default()
    };
    let mut state = [0u8; 256];
    if !unsafe { GetKeyboardState(state.as_mut_ptr()) }.as_bool() {
        return event;
    }
    event.modifiers = i32::from(state[0x10] & 0x80 != 0)
        | (i32::from(state[0x14] & 1 != 0) << 1)
        | (i32::from(state[0x11] & 0x80 != 0) << 2)
        | (i32::from(state[0x12] & 0x80 != 0) << 3)
        | (i32::from(key_up) << 30);
    if vk == 0x14 && !key_up {
        event.modifiers ^= 2;
    }
    event.keycode = special_key(vk, lparam);
    if event.keycode.is_none() {
        // Keep modifiers in the protocol but obtain the printable character.
        for index in [0x11, 0xa2, 0xa3, 0x12, 0xa4, 0xa5] {
            state[index] = 0;
        }
        let mut text = [0u16; 8];
        // Flag 4 preserves the keyboard's dead-key state (Windows 10 1607+).
        let count = unsafe {
            ToUnicodeEx(
                vk,
                ((lparam >> 16) & 0xff) as u32,
                state.as_ptr(),
                windows_core::PWSTR(text.as_mut_ptr()),
                text.len() as i32,
                4,
                Some(GetKeyboardLayout(0)),
            )
        };
        event.keycode = decode_character(&text, count);
    }
    event
}

fn special_key(vk: u32, lparam: i64) -> Option<i32> {
    let extended = lparam & (1 << 24) != 0;
    match vk {
        0x08 => Some(0xff08),
        0x09 => Some(0xff09),
        0x0d => Some(if extended { 0xff8d } else { 0xff0d }),
        0x1b => Some(0xff1b),
        0x21 => Some(0xff55),
        0x22 => Some(0xff56),
        0x23 => Some(0xff57),
        0x24 => Some(0xff50),
        0x25..=0x28 => Some(0xff51 + (vk - 0x25) as i32),
        0x2d => Some(0xff63),
        0x2e => Some(0xffff),
        0x10 => Some(if (lparam >> 16) & 0xff == 0x36 {
            0xffe2
        } else {
            0xffe1
        }),
        0x11 => Some(if extended { 0xffe4 } else { 0xffe3 }),
        0x12 => Some(if extended { 0xffea } else { 0xffe9 }),
        0x14 => Some(0xffe5),
        0x5b => Some(0xffeb),
        0x5c => Some(0xffec),
        0xa0..=0xa5 => Some([0xffe1, 0xffe2, 0xffe3, 0xffe4, 0xffe9, 0xffea][(vk - 0xa0) as usize]),
        0x60..=0x69 => Some(0xffb0 + (vk - 0x60) as i32),
        0x6a..=0x6f => Some(0xffaa + (vk - 0x6a) as i32),
        0x70..=0x87 => Some(0xffbe + (vk - 0x70) as i32),
        _ => None,
    }
}

fn decode_character(text: &[u16], count: i32) -> Option<i32> {
    if count <= 0 {
        return None;
    }
    let decoded = String::from_utf16(text.get(..count as usize)?).ok()?;
    let mut chars = decoded.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(ch as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_uses_layout_output() {
        assert_eq!(special_key(0xbf, 0), None);
        assert_eq!(decode_character(&[b'/' as u16], 1), Some(0x2f));
        assert_eq!(decode_character(&[b'?' as u16], 1), Some(0x3f));
        assert_eq!(special_key(0x6f, 0), Some(0xffaf));
    }

    #[test]
    fn dead_keys_and_multi_character_output_are_not_truncated() {
        assert_eq!(decode_character(&[0x00b4], -1), None);
        assert_eq!(decode_character(&[65, 66], 2), None);
        assert_eq!(decode_character(&[0xd83d, 0xde00], 2), Some(0x1f600));
        assert_eq!(special_key(0x0d, 1 << 24), Some(0xff8d));
    }
}
