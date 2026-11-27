//! The JSON shapes a call's input and answer admit, as data: the Rust equivalent of the TypeBox schemas the SDK root
//! is typed from (`inputs/*.ts`, `results/*.ts`). One [`Shape`] is one schema, [`accepts`] is its `Check`, and
//! [`sample`] is a minimal value it admits, which the call table's tests mutate to prove a field is refused.

use serde_json::{Value, json};

/// One field of an object shape. `optional` is TypeBox's `Type.Optional`: the field may be absent, but if present it
/// must be inside `shape`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub name: &'static str,
    pub shape: Shape,
    pub optional: bool,
}

/// A JSON shape. `Object` is open unless the schema says `additionalProperties: false`, as TypeBox's `Type.Object`
/// is; `Bytes` is the one shape no JSON value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `{}` (TypeBox `Type.Unsafe({})`): any JSON value, null included.
    Any,
    /// An object with any string keys (`Type.Record(Type.String(), Type.Unknown())`).
    AnyObject,
    /// `Type.String()`: any string.
    Text,
    /// `Type.String({ minLength, maxLength })`, counted in characters as JSON Schema counts them.
    Str {
        min: usize,
        max: Option<usize>,
    },
    Bool,
    /// `Type.Number()`: any JSON number.
    Number,
    /// `Type.Integer({ minimum, maximum })`; `1.0` is an integer, as JSON Schema and TypeBox count it.
    Integer {
        min: i64,
        max: Option<i64>,
    },
    /// `Type.Literal("x")`.
    TextLiteral(&'static str),
    /// `Type.Literal(true)` or `Type.Literal(false)`.
    BoolLiteral(bool),
    /// `Type.Literal(4)`; `4.0` is the same literal, as JSON Schema compares numbers.
    IntLiteral(i64),
    Null,
    /// `Type.Union([Type.Boolean(), Type.Null()])`.
    BoolOrNull,
    /// `Type.Enum([...])`.
    Enum(&'static [&'static str]),
    /// `Type.Union([...])`.
    OneOf(&'static [Shape]),
    /// `Type.Array(item, { minItems, maxItems })`.
    Array {
        item: &'static Shape,
        min: usize,
        max: Option<usize>,
    },
    /// `Type.Object(fields)`; `closed` is `additionalProperties: false`.
    Object {
        fields: &'static [FieldSpec],
        closed: bool,
    },
    /// A `Uint8Array` (`Type.Refine(Type.Unsafe<Uint8Array>({}), value => value instanceof Uint8Array)`): no JSON
    /// value is one, so a call whose input requires bytes has no JSON form.
    Bytes,
    /// A stream the caller holds (`Readable`/`ReadableStream`, `Type.Unsafe<ReadableHandle>({ type: "object" })`). The
    /// crate's earlier slice ruled a handle has no JSON form, stricter than TypeBox's `{ type: "object" }`.
    Handle,
    /// A tool name, `^[a-z][a-z0-9_]{1,40}$` (`inputs/threads-authority.ts`).
    ToolName,
}

/// A required field.
pub const fn field(name: &'static str, shape: Shape) -> FieldSpec {
    FieldSpec {
        name,
        shape,
        optional: false,
    }
}

/// An optional field (`Type.Optional`).
pub const fn optional(name: &'static str, shape: Shape) -> FieldSpec {
    FieldSpec {
        name,
        shape,
        optional: true,
    }
}

pub const fn text() -> Shape {
    Shape::Str { min: 0, max: None }
}

pub const fn non_empty() -> Shape {
    Shape::Str { min: 1, max: None }
}

pub const fn min_len(min: usize) -> Shape {
    Shape::Str { min, max: None }
}

pub const fn max_len(max: usize) -> Shape {
    Shape::Str {
        min: 0,
        max: Some(max),
    }
}

pub const fn non_empty_max(max: usize) -> Shape {
    Shape::Str {
        min: 1,
        max: Some(max),
    }
}

pub const fn int_min(min: i64) -> Shape {
    Shape::Integer { min, max: None }
}

pub const fn int_range(min: i64, max: i64) -> Shape {
    Shape::Integer {
        min,
        max: Some(max),
    }
}

pub const fn array(item: &'static Shape, min: usize) -> Shape {
    Shape::Array {
        item,
        min,
        max: None,
    }
}

pub const fn array_range(item: &'static Shape, min: usize, max: usize) -> Shape {
    Shape::Array {
        item,
        min,
        max: Some(max),
    }
}

pub const fn object(fields: &'static [FieldSpec]) -> Shape {
    Shape::Object {
        fields,
        closed: false,
    }
}

pub const fn closed(fields: &'static [FieldSpec]) -> Shape {
    Shape::Object {
        fields,
        closed: true,
    }
}

/// Whether `value` is inside `shape` (TypeBox `Check`).
pub fn accepts(shape: &Shape, value: &Value) -> bool {
    match *shape {
        Shape::Any => true,
        Shape::AnyObject => value.is_object(),
        Shape::Text => value.is_string(),
        Shape::Str { min, max } => value.as_str().is_some_and(|text| {
            let length = text.chars().count();
            length >= min && max.is_none_or(|max| length <= max)
        }),
        Shape::Bool => value.is_boolean(),
        Shape::Number => value.is_number(),
        Shape::Integer { min, max } => value.as_f64().is_some_and(|number| {
            number.fract() == 0.0
                && number >= min as f64
                && max.is_none_or(|max| number <= max as f64)
        }),
        Shape::TextLiteral(text) => value.as_str() == Some(text),
        Shape::BoolLiteral(expected) => value.as_bool() == Some(expected),
        Shape::IntLiteral(expected) => value
            .as_f64()
            .is_some_and(|number| number.fract() == 0.0 && number == expected as f64),
        Shape::Null => value.is_null(),
        Shape::BoolOrNull => value.is_boolean() || value.is_null(),
        Shape::Enum(items) => value.as_str().is_some_and(|text| items.contains(&text)),
        Shape::OneOf(items) => items.iter().any(|item| accepts(item, value)),
        Shape::Array { item, min, max } => value.as_array().is_some_and(|items| {
            items.len() >= min
                && max.is_none_or(|max| items.len() <= max)
                && items.iter().all(|item_value| accepts(item, item_value))
        }),
        Shape::Object { fields, closed } => value.as_object().is_some_and(|object| {
            fields.iter().all(|field| match object.get(field.name) {
                Some(field_value) => accepts(&field.shape, field_value),
                None => field.optional,
            }) && (!closed
                || object
                    .keys()
                    .all(|key| fields.iter().any(|field| field.name == key)))
        }),
        Shape::Bytes | Shape::Handle => false,
        Shape::ToolName => value.as_str().is_some_and(is_tool_name),
    }
}

fn is_tool_name(name: &str) -> bool {
    let mut characters = name.chars();
    if !characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
    {
        return false;
    }
    let rest = characters.count();
    (1..=40).contains(&rest)
        && name.chars().skip(1).all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

/// A minimal value `shape` admits: every required field, no optional one, and the first member of a union. `None`
/// when a required field is [`Shape::Bytes`], which no JSON value is.
pub fn sample(shape: &Shape) -> Option<Value> {
    Some(match *shape {
        Shape::Any => Value::Null,
        Shape::AnyObject => json!({}),
        Shape::Text => json!(""),
        Shape::Str { min, max } => {
            let length = min.min(max.unwrap_or(usize::MAX));
            json!("x".repeat(length))
        }
        Shape::Bool => json!(true),
        Shape::Number => json!(1),
        Shape::Integer { min, .. } => json!(min),
        Shape::TextLiteral(text) => json!(text),
        Shape::BoolLiteral(expected) => json!(expected),
        Shape::IntLiteral(expected) => json!(expected),
        Shape::Null => Value::Null,
        Shape::BoolOrNull => json!(true),
        Shape::Enum(items) => json!(items.first()?),
        Shape::OneOf(items) => items.iter().find_map(sample)?,
        Shape::Array { item, min, .. } => {
            let mut items = Vec::new();
            for _ in 0..min {
                items.push(sample(item)?);
            }
            Value::Array(items)
        }
        Shape::Object { fields, .. } => {
            let mut object = serde_json::Map::new();
            for field in fields.iter().filter(|field| !field.optional) {
                object.insert(field.name.to_owned(), sample(&field.shape)?);
            }
            Value::Object(object)
        }
        Shape::Bytes | Shape::Handle => return None,
        Shape::ToolName => json!("ab"),
    })
}

/// Whether every field of an object shape is optional, so the call may omit its input (`Partial<T> extends T`). A
/// union is omittable only when every member is; a shape that is not an object is not.
pub fn all_fields_optional(shape: &Shape) -> bool {
    match shape {
        Shape::Object { fields, .. } => fields.iter().all(|field| field.optional),
        Shape::OneOf(items) => items.iter().all(all_fields_optional),
        _ => false,
    }
}

/// Whether an object shape names `name` at its top level (a union: in any member).
pub fn names_field(shape: &Shape, name: &str) -> bool {
    match shape {
        Shape::Object { fields, .. } => fields.iter().any(|field| field.name == name),
        Shape::OneOf(items) => items.iter().any(|item| names_field(item, name)),
        _ => false,
    }
}
