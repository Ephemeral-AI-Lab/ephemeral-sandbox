use serde_json::Value;

pub fn encode_json_line(value: &Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).unwrap_or_default();
    push_json_line_delimiter(&mut line);
    line
}

fn push_json_line_delimiter(line: &mut Vec<u8>) {
    line.push(b'\n');
}
