//! data structure used on the wire by Cloud Datastore APIs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;


// Lookup API

#[derive(Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum LookupResult {
    Found { found: Vec<EntityResult> },
    Missing { missing: Vec<EntityResult> },
}

impl LookupResult {
    pub(crate) fn as_option(self) -> Option<Vec<EntityResult>> {
        match self {
            LookupResult::Found { found } => Some(found),
            LookupResult::Missing { .. } => None,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct EntityResult {
    pub entity: Entity,
    pub version: String,
    pub cursor: Option<String>,
}

// Common structures (entity, key, values)

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Entity {
    pub key: Key,
    #[serde(default)]
    pub properties: HashMap<String, Value>,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    pub partition_id: PartitionId,
    pub path: Vec<PathElement>,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionId {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    pub project_id: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct PathElement {
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Value {
    #[serde(flatten)]
    value: ValueField,
}

impl Value {
    pub fn new_integer(v: u32, exclude_from_indexes: Option<bool>) -> Value {
        Value {
            value: ValueField::Integer {
                exclude_from_indexes,
                integer_value: v.to_string(),
            },
        }
    }

    pub fn new_string(v: &str, exclude_from_indexes: Option<bool>) -> Value {
        Value {
            value: ValueField::String {
                exclude_from_indexes,
                string_value: v.to_string(),
            },
        }
    }

    pub fn new_array<I: Iterator<Item = Value>>(i: I) -> Value {
        Value {
            value: ValueField::Array {
                array_value: ArrayValue {
                    values: i.collect(),
                },
            },
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match &self.value {
            ValueField::Integer { integer_value, .. } => integer_value.parse::<u32>().ok(),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<String> {
        match &self.value {
            ValueField::String { string_value, .. } => Some(string_value.clone()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&ArrayValue> {
        match &self.value {
            ValueField::Array { array_value } => Some(array_value),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
enum ValueField {
    #[serde(rename = "string_value")]
    #[serde(rename_all = "camelCase")]
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        exclude_from_indexes: Option<bool>,
        string_value: String,
    },
    #[serde(rename = "integer_value")]
    #[serde(rename_all = "camelCase")]
    Integer {
        #[serde(skip_serializing_if = "Option::is_none")]
        exclude_from_indexes: Option<bool>,
        integer_value: String,
    },
    #[serde(rename = "array_value")]
    #[serde(rename_all = "camelCase")]
    Array { array_value: ArrayValue },
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct ArrayValue {
    pub values: Vec<Value>,
}

// Begin Transaction

#[derive(Debug, Deserialize, PartialEq)]
pub struct BeginTransactionResponse {
    pub transaction: TransactionId,
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct TransactionId(String);


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn asset(p: &str) -> String {
        let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        d.push("assets");
        d.push(p);

        std::fs::read_to_string(d).unwrap()
    }

    fn entity_result(properties: HashMap<String, Value>) -> EntityResult {
        let entity = Entity {
            key: Key {
                partition_id: PartitionId {
                    namespace: None,
                    project_id: "pid".to_string(),
                },
                path: vec![PathElement {
                    kind: "oauth2token".to_string(),
                    name: "my@gmail.com".to_string(),
                }],
            },
            properties,
        };

        EntityResult {
            entity,
            version: "2907302240639813".to_string(),
            cursor: None,
        }
    }

    #[test]
    fn deserialize_datastore_found() {
        let body = asset("test_datastore_found_response.json");

        let actual: LookupResult = serde_json::from_str(&body).unwrap();

        let mut properties = HashMap::new();

        properties.insert(
            "token".to_string(),
            Value::new_string(
                "LvrNooPprYvSiVwyN3VRIARnc05Pte/dtENtlLpWPZ7cC0O",
                Some(true),
            ),
        );

        properties.insert(
            "scopes".to_string(),
            Value::new_array(
                vec![
                    Value::new_string("email", None),
                    Value::new_string("profile", None),
                ]
                .into_iter(),
            ),
        );

        let expected = LookupResult::Found {
            found: vec![entity_result(properties)],
        };

        assert_eq!(actual, expected);
    }

    #[test]
    fn deserialize_datastore_missing() {
        let body = asset("test_datastore_missing_response.json");

        let actual: LookupResult = serde_json::from_str(&body).unwrap();

        let expected = LookupResult::Missing {
            missing: vec![entity_result(HashMap::new())],
        };

        assert_eq!(actual, expected);
    }
}