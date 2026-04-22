use cel::common::types::{CelBool, CelBytes, CelDouble, CelInt, CelString, CelStruct, CelUInt};
use cel::{Env, StructDef, Value};
use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind as ProtoKind, MessageDescriptor, Value as ProtoValue,
};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

#[derive(Debug)]
pub enum ConversionError {
    TypeMismatch {
        field: String,
        expected: String,
        got: String,
    },
    UnsupportedFieldType {
        field: String,
        kind: String,
    },
    NotAStruct,
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConversionError::TypeMismatch {
                field,
                expected,
                got,
            } => write!(
                f,
                "Type mismatch for field '{}': expected {}, got {}",
                field, expected, got
            ),
            ConversionError::UnsupportedFieldType { field, kind } => {
                write!(f, "Unsupported field type for '{}': {}", field, kind)
            }
            ConversionError::NotAStruct => write!(f, "CEL value is not a struct"),
        }
    }
}

impl std::error::Error for ConversionError {}

/// Converts protobuf MessageDescriptors to CEL StructDefs
pub struct DescriptorConverter;

impl DescriptorConverter {
    /// Register a message descriptor and all its nested message types with the CEL environment
    /// This must be called before evaluating CEL expressions that construct these messages
    pub fn register_message_types(
        env: &mut Env,
        descriptor: &MessageDescriptor,
    ) -> Result<(), ConversionError> {
        let mut to_register = vec![descriptor.clone()];
        let mut visited = HashSet::new();

        while let Some(desc) = to_register.pop() {
            if !visited.insert(desc.full_name().to_string()) {
                continue;
            }

            for field in desc.fields() {
                if let ProtoKind::Message(nested_desc) = field.kind() {
                    to_register.push(nested_desc);
                }
            }

            let struct_def = Self::to_struct_def(&desc)?;
            env.add_struct(struct_def);
        }

        Ok(())
    }

    /// Convert a protobuf MessageDescriptor to a CEL StructDef
    /// Supports nested messages - they will be referenced by name
    pub fn to_struct_def(descriptor: &MessageDescriptor) -> Result<StructDef, ConversionError> {
        let mut struct_def = StructDef::new(descriptor.full_name().to_string());

        for field in descriptor.fields() {
            let cel_type = Self::protobuf_kind_to_cel_type(field.kind())?;
            struct_def = struct_def.add_field(field.name().to_string(), cel_type);
        }

        Ok(struct_def)
    }

    fn protobuf_kind_to_cel_type(
        kind: ProtoKind,
    ) -> Result<cel::common::types::Type, ConversionError> {
        use cel::common::types::*;

        match kind {
            ProtoKind::Bool => Ok(BOOL_TYPE),
            ProtoKind::Int32
            | ProtoKind::Int64
            | ProtoKind::Sint32
            | ProtoKind::Sint64
            | ProtoKind::Sfixed32
            | ProtoKind::Sfixed64 => Ok(INT_TYPE),
            ProtoKind::Uint32 | ProtoKind::Uint64 | ProtoKind::Fixed32 | ProtoKind::Fixed64 => {
                Ok(UINT_TYPE)
            }
            ProtoKind::Float | ProtoKind::Double => Ok(DOUBLE_TYPE),
            ProtoKind::String => Ok(STRING_TYPE),
            ProtoKind::Bytes => Ok(BYTES_TYPE),
            ProtoKind::Message(desc) => Ok(Type::new_struct(desc.full_name().to_string())),
            ProtoKind::Enum(_) => {
                // Enums are represented as INT in CEL
                Ok(INT_TYPE)
            }
        }
    }
}

pub struct MessageConverter;

impl MessageConverter {
    pub fn cel_to_dynamic_message(
        cel_value: &Value,
        descriptor: &MessageDescriptor,
    ) -> Result<DynamicMessage, ConversionError> {
        match cel_value {
            Value::Struct(cel_struct) => Self::struct_to_dynamic_message(cel_struct, descriptor),
            _ => Err(ConversionError::NotAStruct),
        }
    }

    fn struct_to_dynamic_message(
        cel_struct: &Arc<CelStruct>,
        descriptor: &MessageDescriptor,
    ) -> Result<DynamicMessage, ConversionError> {
        let mut message = DynamicMessage::new(descriptor.clone());

        for field in descriptor.fields() {
            if let Some(val) = cel_struct.field_value(field.name()) {
                let proto_value = Self::cel_val_to_proto_value(val, &field)?;
                message.set_field(&field, proto_value);
            }
        }

        Ok(message)
    }

    fn cel_val_to_proto_value(
        cel_val: &dyn cel::common::value::Val,
        field: &FieldDescriptor,
    ) -> Result<ProtoValue, ConversionError> {
        let field_name = field.name();

        match field.kind() {
            ProtoKind::Bool => {
                let b = cel_val.downcast_ref::<CelBool>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "bool".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::Bool(*b.inner()))
            }
            ProtoKind::Int32 | ProtoKind::Sint32 | ProtoKind::Sfixed32 => {
                let i = cel_val.downcast_ref::<CelInt>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "int".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::I32(*i.inner() as i32))
            }
            ProtoKind::Int64 | ProtoKind::Sint64 | ProtoKind::Sfixed64 => {
                let i = cel_val.downcast_ref::<CelInt>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "int".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::I64(*i.inner()))
            }
            ProtoKind::Uint32 | ProtoKind::Fixed32 => {
                let u = cel_val.downcast_ref::<CelUInt>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "uint".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::U32(*u.inner() as u32))
            }
            ProtoKind::Uint64 | ProtoKind::Fixed64 => {
                let u = cel_val.downcast_ref::<CelUInt>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "uint".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::U64(*u.inner()))
            }
            ProtoKind::Float => {
                let f = cel_val.downcast_ref::<CelDouble>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "float".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                let f64_value = *f.inner();
                if !f64_value.is_finite()
                    || f64_value < f32::MIN as f64
                    || f64_value > f32::MAX as f64
                {
                    return Err(ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "float value within f32 range".to_string(),
                        got: format!("{}", f64_value),
                    });
                }
                Ok(ProtoValue::F32(f64_value as f32))
            }
            ProtoKind::Double => {
                let f = cel_val.downcast_ref::<CelDouble>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "double".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::F64(*f.inner()))
            }
            ProtoKind::String => {
                let s = cel_val.downcast_ref::<CelString>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "string".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::String(s.inner().to_string()))
            }
            ProtoKind::Bytes => {
                let b = cel_val.downcast_ref::<CelBytes>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "bytes".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                Ok(ProtoValue::Bytes(b.inner().to_vec().into()))
            }
            ProtoKind::Enum(enum_desc) => {
                // CEL represents enums as integers
                let i = cel_val.downcast_ref::<CelInt>().ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "int (enum)".to_string(),
                        got: format!("{:?}", cel_val),
                    }
                })?;
                let i64_value = *i.inner();
                if i64_value < i32::MIN as i64 || i64_value > i32::MAX as i64 {
                    return Err(ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: "int32 value for enum".to_string(),
                        got: format!("{}", i64_value),
                    });
                }
                let value = enum_desc.get_value(i64_value as i32).ok_or_else(|| {
                    ConversionError::TypeMismatch {
                        field: field_name.to_string(),
                        expected: format!("valid enum value for {}", enum_desc.name()),
                        got: format!("{}", i64_value),
                    }
                })?;
                Ok(ProtoValue::EnumNumber(value.number()))
            }
            ProtoKind::Message(nested_desc) => {
                // Nested message - recurse
                let nested_message =
                    Self::struct_from_val_to_message(cel_val, &nested_desc, &field_name)?;
                Ok(ProtoValue::Message(nested_message))
            }
        }
    }

    fn struct_from_val_to_message(
        cel_val: &dyn cel::common::value::Val,
        descriptor: &MessageDescriptor,
        field_name: &str,
    ) -> Result<DynamicMessage, ConversionError> {
        let nested_struct =
            cel_val
                .downcast_ref::<CelStruct>()
                .ok_or_else(|| ConversionError::TypeMismatch {
                    field: field_name.to_string(),
                    expected: "struct (nested message)".to_string(),
                    got: format!("{:?}", cel_val),
                })?;

        let mut message = DynamicMessage::new(descriptor.clone());

        for field in descriptor.fields() {
            if let Some(val) = nested_struct.field_value(field.name()) {
                let proto_value = Self::cel_val_to_proto_value(val, &field)?;
                message.set_field(&field, proto_value);
            }
        }

        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel::{Context, Program};
    use prost::Message;
    use prost_types::{field_descriptor_proto, DescriptorProto, FieldDescriptorProto};
    use prost_types::{FileDescriptorProto, FileDescriptorSet};
    use std::sync::Arc;

    fn create_test_message_descriptor() -> MessageDescriptor {
        let file_descriptor = FileDescriptorProto {
            name: Some("test.proto".to_string()),
            package: Some("test".to_string()),
            message_type: vec![DescriptorProto {
                name: Some("TestMessage".to_string()),
                field: vec![
                    FieldDescriptorProto {
                        name: Some("string_field".to_string()),
                        number: Some(1),
                        r#type: Some(field_descriptor_proto::Type::String.into()),
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("int32_field".to_string()),
                        number: Some(2),
                        r#type: Some(field_descriptor_proto::Type::Int32.into()),
                        ..Default::default()
                    },
                    FieldDescriptorProto {
                        name: Some("bool_field".to_string()),
                        number: Some(3),
                        r#type: Some(field_descriptor_proto::Type::Bool.into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        let fds = FileDescriptorSet {
            file: vec![file_descriptor],
        };

        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(fds)
            .expect("Failed to create descriptor pool");

        pool.get_message_by_name("test.TestMessage")
            .expect("Message not found")
    }

    #[test]
    fn test_descriptor_to_struct_def() {
        let descriptor = create_test_message_descriptor();
        let struct_def = DescriptorConverter::to_struct_def(&descriptor);

        assert!(struct_def.is_ok());
        let struct_def = struct_def.unwrap();

        // Verify we can register it with CEL
        let mut env = cel::Env::stdlib();
        env.add_struct(struct_def);

        // Should be able to compile an expression using the struct
        let result = Program::compile(
            "test.TestMessage { string_field: 'hello', int32_field: 42, bool_field: true }",
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_cel_to_dynamic_message() {
        let descriptor = create_test_message_descriptor();
        let struct_def =
            DescriptorConverter::to_struct_def(&descriptor).expect("Failed to convert descriptor");

        let mut env = cel::Env::stdlib();
        env.add_struct(struct_def);

        let ctx = Context::with_env(Arc::new(env));

        let program = Program::compile(
            "test.TestMessage { string_field: 'hello', int32_field: 42, bool_field: true }",
        )
        .expect("Failed to compile");

        let cel_value = program.execute(&ctx).expect("Failed to execute");

        let message = MessageConverter::cel_to_dynamic_message(&cel_value, &descriptor)
            .expect("Failed to convert");

        // Verify fields
        let string_field = message
            .get_field_by_name("string_field")
            .expect("string_field not found");
        assert_eq!(
            string_field.as_ref(),
            &ProtoValue::String("hello".to_string())
        );

        let int_field = message
            .get_field_by_name("int32_field")
            .expect("int32_field not found");
        assert_eq!(int_field.as_ref(), &ProtoValue::I32(42));

        let bool_field = message
            .get_field_by_name("bool_field")
            .expect("bool_field not found");
        assert_eq!(bool_field.as_ref(), &ProtoValue::Bool(true));
    }

    #[test]
    fn test_nested_messages() {
        // Create a descriptor with nested messages
        let file_descriptor = FileDescriptorProto {
            name: Some("test.proto".to_string()),
            package: Some("test".to_string()),
            message_type: vec![
                DescriptorProto {
                    name: Some("Inner".to_string()),
                    field: vec![FieldDescriptorProto {
                        name: Some("value".to_string()),
                        number: Some(1),
                        r#type: Some(field_descriptor_proto::Type::String.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                DescriptorProto {
                    name: Some("Outer".to_string()),
                    field: vec![
                        FieldDescriptorProto {
                            name: Some("name".to_string()),
                            number: Some(1),
                            r#type: Some(field_descriptor_proto::Type::String.into()),
                            ..Default::default()
                        },
                        FieldDescriptorProto {
                            name: Some("inner".to_string()),
                            number: Some(2),
                            r#type: Some(field_descriptor_proto::Type::Message.into()),
                            type_name: Some(".test.Inner".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let fds = FileDescriptorSet {
            file: vec![file_descriptor],
        };

        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(fds)
            .expect("Failed to create descriptor pool");

        let outer_descriptor = pool
            .get_message_by_name("test.Outer")
            .expect("Outer message not found");

        // Register all message types
        let mut env = cel::Env::stdlib();
        DescriptorConverter::register_message_types(&mut env, &outer_descriptor)
            .expect("Failed to register types");

        let ctx = Context::with_env(Arc::new(env));

        // Build a CEL expression with nested message
        let program = Program::compile(
            r#"test.Outer { name: "parent", inner: test.Inner { value: "child" } }"#,
        )
        .expect("Failed to compile");

        let cel_value = program.execute(&ctx).expect("Failed to execute");

        // Convert to DynamicMessage
        let message = MessageConverter::cel_to_dynamic_message(&cel_value, &outer_descriptor)
            .expect("Failed to convert");

        // Verify outer fields
        let name_field = message
            .get_field_by_name("name")
            .expect("name field not found");
        assert_eq!(
            name_field.as_ref(),
            &ProtoValue::String("parent".to_string())
        );

        // Verify nested message
        let inner_field = message
            .get_field_by_name("inner")
            .expect("inner field not found");
        if let ProtoValue::Message(inner_msg) = inner_field.as_ref() {
            let value_field = inner_msg
                .get_field_by_name("value")
                .expect("value field not found");
            assert_eq!(
                value_field.as_ref(),
                &ProtoValue::String("child".to_string())
            );
        } else {
            panic!("inner field is not a message");
        }
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let descriptor = create_test_message_descriptor();
        let struct_def =
            DescriptorConverter::to_struct_def(&descriptor).expect("Failed to convert descriptor");

        let mut env = cel::Env::stdlib();
        env.add_struct(struct_def);

        let ctx = Context::with_env(Arc::new(env));

        let program = Program::compile(
            "test.TestMessage { string_field: 'test', int32_field: 123, bool_field: false }",
        )
        .expect("Failed to compile");

        let cel_value = program.execute(&ctx).expect("Failed to execute");

        let message = MessageConverter::cel_to_dynamic_message(&cel_value, &descriptor)
            .expect("Failed to convert");

        // Encode to protobuf bytes
        let mut bytes = Vec::new();
        message.encode(&mut bytes).expect("Failed to encode");

        // Decode back
        let decoded =
            DynamicMessage::decode(descriptor, bytes.as_slice()).expect("Failed to decode");

        // Verify fields match
        assert_eq!(
            message.get_field_by_name("string_field"),
            decoded.get_field_by_name("string_field")
        );
        assert_eq!(
            message.get_field_by_name("int32_field"),
            decoded.get_field_by_name("int32_field")
        );
        assert_eq!(
            message.get_field_by_name("bool_field"),
            decoded.get_field_by_name("bool_field")
        );
    }
}
