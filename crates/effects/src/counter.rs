//! Deterministic original counter formatting, shared by Numbers and ordinary text counter rigs.

pub fn number(value: f64, decimals: usize, padding: usize, grouping: u32) -> String {
    if !value.is_finite() {
        return "Invalid".into();
    }
    let text = format!("{:.*}", decimals.min(10), value.abs());
    let (integer, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let integer = format!("{:0>width$}", integer, width = padding.min(32));
    let separator = match grouping {
        1 => Some(','),
        2 => Some('.'),
        3 => Some(' '),
        _ => None,
    };
    let mut result = String::new();
    if value.is_sign_negative() {
        result.push('-');
    }
    for (index, digit) in integer.chars().enumerate() {
        if index > 0
            && (integer.len() - index).is_multiple_of(3)
            && let Some(separator) = separator
        {
            result.push(separator);
        }
        result.push(digit);
    }
    if !fraction.is_empty() {
        result.push(if grouping == 2 { ',' } else { '.' });
        result.push_str(fraction);
    }
    result
}

pub fn clock(value: f64, decimals: usize, style: u32) -> String {
    if !value.is_finite() {
        return "Invalid".into();
    }
    let precision = decimals.min(6);
    let rate = 10u128.pow(precision as u32);
    // i128 intermediates and unsigned_abs also cover adversarial numeric values without overflow.
    let ticks = (value * rate as f64).floor() as i128;
    let absolute = ticks.unsigned_abs();
    let seconds = absolute / rate;
    let mut result = match style {
        1 => format!("{:02}:{:02}", seconds / 60, seconds % 60),
        2 => format!("{seconds}"),
        _ => format!("{:02}:{:02}:{:02}", seconds / 3600, (seconds / 60) % 60, seconds % 60),
    };
    if precision > 0 {
        result.push_str(&format!(".{:0>width$}", absolute % rate, width = precision));
    }
    if ticks < 0 {
        result.insert(0, '-');
    }
    result
}

pub fn affixes(text: String, prefix: &str, suffix: &str) -> String {
    // User strings from project files must not allocate an unbounded glyph list each frame.
    let prefix: String = prefix.chars().take(256).collect();
    let suffix: String = suffix.chars().take(256).collect();
    format!("{prefix}{text}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sign_precision_padding_and_three_separator_styles() {
        assert_eq!(number(-1234567.125, 2, 0, 1), "-1,234,567.12");
        assert_eq!(number(1234.5, 2, 0, 2), "1.234,50");
        assert_eq!(number(1234.5, 1, 0, 3), "1 234.5");
        assert_eq!(number(-7.25, 2, 5, 0), "-00007.25");
        assert_eq!(clock(3661.125, 3, 0), "01:01:01.125");
        assert_eq!(clock(61.5, 1, 1), "01:01.5");
        assert_eq!(clock(-0.5, 2, 2), "-0.50");
        assert_eq!(number(f64::NAN, 2, 0, 0), "Invalid");
        assert!(clock(-1e300, 6, 0).starts_with('-'));
        assert!(affixes("7".into(), &"😀".repeat(10_000), "").chars().count() <= 257);
    }
}
