//! 规范化 JSON：键按字典序、2 空格缩进、UTF-8、文件末尾一个换行、只允许整数。

use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum CanonError {
    #[error("规范化输出中出现浮点数：{0}")]
    Float(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub fn to_bytes(v: &Value) -> Result<Vec<u8>, CanonError> {
    reject_floats(v, "$")?;
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    Ok(s.into_bytes())
}

fn reject_floats(v: &Value, path: &str) -> Result<(), CanonError> {
    match v {
        Value::Number(n) if !(n.is_i64() || n.is_u64()) => {
            Err(CanonError::Float(format!("{path} = {n}")))
        }
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                reject_floats(x, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (k, x) in map {
                reject_floats(x, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_sorted_and_trailing_newline() {
        // 如果某个依赖开启了 serde_json 的 preserve_order，这个测试会失败
        let v = json!({"b": 1, "a": {"d": 2, "c": "中文"}});
        let s = String::from_utf8(to_bytes(&v).unwrap()).unwrap();
        assert_eq!(
            s,
            "{\n  \"a\": {\n    \"c\": \"中文\",\n    \"d\": 2\n  },\n  \"b\": 1\n}\n"
        );
    }

    #[test]
    fn floats_rejected() {
        assert!(to_bytes(&json!({"x": [1, 0.5]})).is_err());
    }
}
