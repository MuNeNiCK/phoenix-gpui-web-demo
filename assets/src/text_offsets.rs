pub(crate) fn contiguous_diff(current: &str, next: &str) -> (u32, u32, String) {
    let current_chars: Vec<char> = current.chars().collect();
    let next_chars: Vec<char> = next.chars().collect();
    let mut prefix = 0;

    while prefix < current_chars.len()
        && prefix < next_chars.len()
        && current_chars[prefix] == next_chars[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < current_chars.len() - prefix
        && suffix < next_chars.len() - prefix
        && current_chars[current_chars.len() - 1 - suffix]
            == next_chars[next_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let index = utf16_len(&current_chars[..prefix]);
    let removed = utf16_len(&current_chars[prefix..current_chars.len() - suffix]);
    let inserted = next_chars[prefix..next_chars.len() - suffix]
        .iter()
        .collect();
    (index, removed, inserted)
}

fn utf16_len(chars: &[char]) -> u32 {
    chars
        .iter()
        .map(|character| character.len_utf16() as u32)
        .sum()
}

pub(crate) fn byte_offset_to_utf16(value: &str, offset: usize) -> u32 {
    let mut offset = offset.min(value.len());
    while !value.is_char_boundary(offset) {
        offset -= 1;
    }
    value[..offset].encode_utf16().count() as u32
}

pub(crate) fn utf16_offset_to_byte(value: &str, offset: u32) -> usize {
    let mut utf16_offset = 0;
    for (byte_offset, character) in value.char_indices() {
        if utf16_offset >= offset {
            return byte_offset;
        }
        utf16_offset += character.len_utf16() as u32;
    }
    value.len()
}

#[cfg(test)]
mod tests {
    use super::contiguous_diff;

    #[test]
    fn calculates_utf16_text_differences() {
        assert_eq!(contiguous_diff("hello", "help"), (3, 2, "p".to_string()));
        assert_eq!(contiguous_diff("a😀b", "a🌱b"), (1, 2, "🌱".to_string()));
        assert_eq!(contiguous_diff("same", "same"), (4, 0, String::new()));
    }
}
