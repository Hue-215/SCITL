use serde_json::{Map, Value};

use crate::error::{CoreError, Result};

/// ツール引数の検証(docs/spec/rebuild/tools.md 3節)。各ツールは受ける引数の名前と型だけを
/// 書き、検証の規則はここに閉じる。型が期待と違う場合は変換を試みずエラーにする
/// (docs/spec/principles.md 3節)。
pub(super) struct Args<'a> {
    object: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    /// 引数がJSONオブジェクトであること、`known`に無い引数を含まないことを確かめる。
    pub fn parse(arguments: &'a Value, known: &[&str]) -> Result<Self> {
        let object = arguments
            .as_object()
            .ok_or_else(|| invalid("arguments", "expected a JSON object"))?;
        if let Some(key) = object.keys().find(|key| !known.contains(&key.as_str())) {
            return Err(CoreError::UnknownArgument(key.clone()));
        }
        Ok(Self { object })
    }

    /// 省略と`null`はどちらも`None`。
    pub fn optional_string(&self, name: &str) -> Result<Option<String>> {
        match self.object.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(invalid(name, "expected a string")),
        }
    }

    /// 省略と`null`はどちらも`None`。
    pub fn optional_bool(&self, name: &str) -> Result<Option<bool>> {
        match self.object.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(invalid(name, "expected a boolean")),
        }
    }

    pub fn required_i64(&self, name: &str) -> Result<i64> {
        self.required(name)?
            .as_i64()
            .ok_or_else(|| invalid(name, "expected an integer"))
    }

    /// 空の配列は拒否する。
    pub fn required_string_array(&self, name: &str) -> Result<Vec<String>> {
        let items = string_array(self.required(name)?, name)?;
        if items.is_empty() {
            return Err(invalid(name, "must not be empty"));
        }
        Ok(items)
    }

    /// 省略と`null`はどちらも`None`。空の配列はそのまま受ける。
    pub fn optional_string_array(&self, name: &str) -> Result<Option<Vec<String>>> {
        match self.object.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => string_array(value, name).map(Some),
        }
    }

    fn required(&self, name: &str) -> Result<&'a Value> {
        self.object
            .get(name)
            .ok_or_else(|| invalid(name, "required"))
    }
}

fn string_array(value: &Value, name: &str) -> Result<Vec<String>> {
    let expected = || invalid(name, "expected an array of strings");
    value
        .as_array()
        .ok_or_else(expected)?
        .iter()
        .map(|item| item.as_str().map(str::to_string).ok_or_else(expected))
        .collect()
}

fn invalid(name: &str, reason: &str) -> CoreError {
    CoreError::InvalidArgument {
        name: name.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_non_object() {
        let err = Args::parse(&json!([]), &[]).err().unwrap();
        assert!(matches!(err, CoreError::InvalidArgument { name, .. } if name == "arguments"));
    }

    #[test]
    fn rejects_unknown_argument() {
        let err = Args::parse(&json!({ "a": 1, "b": 2 }), &["a"])
            .err()
            .unwrap();
        assert!(matches!(err, CoreError::UnknownArgument(key) if key == "b"));
    }

    #[test]
    fn optional_treats_null_as_absent() {
        let value = json!({ "s": null });
        let args = Args::parse(&value, &["s", "b"]).unwrap();
        assert_eq!(args.optional_string("s").unwrap(), None);
        assert_eq!(args.optional_bool("b").unwrap(), None);
    }

    #[test]
    fn does_not_coerce_types() {
        let value = json!({ "s": 1, "b": "true", "i": "1" });
        let args = Args::parse(&value, &["s", "b", "i"]).unwrap();
        assert!(args.optional_string("s").is_err());
        assert!(args.optional_bool("b").is_err());
        assert!(args.required_i64("i").is_err());
    }

    #[test]
    fn required_reports_missing() {
        let value = json!({});
        let args = Args::parse(&value, &["i"]).unwrap();
        let err = args.required_i64("i").unwrap_err();
        assert!(matches!(err, CoreError::InvalidArgument { reason, .. } if reason == "required"));
    }

    #[test]
    fn optional_string_array_accepts_empty_and_absent() {
        let value = json!({ "a": [], "n": null });
        let args = Args::parse(&value, &["a", "n", "m"]).unwrap();
        assert_eq!(args.optional_string_array("a").unwrap(), Some(Vec::new()));
        assert_eq!(args.optional_string_array("n").unwrap(), None);
        assert_eq!(args.optional_string_array("m").unwrap(), None);
        let value = json!({ "a": "deadline" });
        let args = Args::parse(&value, &["a"]).unwrap();
        assert!(args.optional_string_array("a").is_err());
    }

    #[test]
    fn string_array_rejects_empty_and_non_string_items() {
        let empty = json!({ "a": [] });
        assert!(Args::parse(&empty, &["a"])
            .unwrap()
            .required_string_array("a")
            .is_err());
        let mixed = json!({ "a": ["x", 1] });
        assert!(Args::parse(&mixed, &["a"])
            .unwrap()
            .required_string_array("a")
            .is_err());
    }
}
