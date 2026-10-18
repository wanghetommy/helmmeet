/// Resuming must neither silently unmute nor select a different default input.
pub fn resume_input(
    mask: i32,
    requested: u32,
    previous: u32,
    was_disabled: bool,
    confirm_microphone: bool,
) -> Result<u32, &'static str> {
    if mask & 1 != 0 && was_disabled && !confirm_microphone {
        return Err("麦克风此前已关闭，请明确确认恢复，或选择仅电脑声音继续");
    }
    Ok(if mask & 1 != 0 && requested == 0 && previous != 0 {
        previous
    } else {
        requested
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_preserves_mute_and_original_device() {
        assert!(resume_input(3, 0, 42, true, false).is_err());
        assert_eq!(resume_input(3, 0, 42, true, true), Ok(42));
        assert_eq!(resume_input(3, 99, 42, false, false), Ok(99));
        assert_eq!(resume_input(2, 0, 42, true, false), Ok(0));
    }
}
