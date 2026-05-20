//! 工具定义分级压缩模块
//!
//! 根据工具定义总大小自动选择压缩级别：
//! - Level 0 (< 20KB): 不压缩
//! - Level 1 (20-40KB): 移除 property descriptions，保留 tool description
//! - Level 2 (40-80KB): 移除所有 descriptions，只留 name + param names + types
//! - Level 3 (> 80KB): Level 2 + 折叠 enum 为摘要
//!
//! 也支持手动指定级别: "off"/"level1"/"level2"/"level3"/"auto"

use crate::kiro::model::requests::tool::{InputSchema, Tool as KiroTool, ToolSpecification};

const THRESHOLD_L1: usize = 20 * 1024;
const THRESHOLD_L2: usize = 40 * 1024;
const THRESHOLD_L3: usize = 80 * 1024;

const MIN_DESCRIPTION_CHARS: usize = 50;

/// 根据配置级别压缩工具定义
pub fn compress_tools_if_needed(tools: &[KiroTool], level: &str) -> Vec<KiroTool> {
    if level == "off" {
        return tools.to_vec();
    }

    let total_size = estimate_tools_size(tools);

    let effective_level = match level {
        "level1" => 1,
        "level2" => 2,
        "level3" => 3,
        _ => {
            // auto
            if total_size <= THRESHOLD_L1 {
                return tools.to_vec();
            } else if total_size <= THRESHOLD_L2 {
                1
            } else if total_size <= THRESHOLD_L3 {
                2
            } else {
                3
            }
        }
    };

    tracing::info!(
        total_size,
        effective_level,
        tool_count = tools.len(),
        "工具定义压缩: level {}",
        effective_level
    );

    match effective_level {
        1 => compress_level1(tools, total_size),
        2 => compress_level2(tools),
        3 => compress_level3(tools),
        _ => tools.to_vec(),
    }
}

/// Level 1: 移除 property descriptions，保留 tool description
/// 如果仍超阈值，按比例截断 tool description
fn compress_level1(tools: &[KiroTool], original_size: usize) -> Vec<KiroTool> {
    let mut compressed: Vec<KiroTool> = tools.iter().map(|t| simplify_schema(t, false)).collect();

    let size_after = estimate_tools_size(&compressed);
    if size_after <= THRESHOLD_L1 {
        return compressed;
    }

    // 按比例截断 description
    let ratio = THRESHOLD_L1 as f64 / size_after as f64;
    truncate_descriptions(&mut compressed, ratio);

    tracing::info!(
        original_size,
        final_size = estimate_tools_size(&compressed),
        "Level 1 压缩完成"
    );
    compressed
}

/// Level 2: 移除所有 descriptions（tool + property），只留 name + params + types
fn compress_level2(tools: &[KiroTool]) -> Vec<KiroTool> {
    tools
        .iter()
        .map(|t| {
            let simplified = simplify_schema(t, false);
            KiroTool {
                tool_specification: ToolSpecification {
                    name: simplified.tool_specification.name.clone(),
                    description: simplified.tool_specification.name.clone(),
                    input_schema: simplified.tool_specification.input_schema,
                },
            }
        })
        .collect()
}

/// Level 3: Level 2 + 折叠 enum 为摘要
fn compress_level3(tools: &[KiroTool]) -> Vec<KiroTool> {
    tools
        .iter()
        .map(|t| {
            let simplified = simplify_schema(t, true);
            KiroTool {
                tool_specification: ToolSpecification {
                    name: simplified.tool_specification.name.clone(),
                    description: simplified.tool_specification.name.clone(),
                    input_schema: simplified.tool_specification.input_schema,
                },
            }
        })
        .collect()
}

/// 按比例截断所有 tool descriptions
fn truncate_descriptions(tools: &mut [KiroTool], ratio: f64) {
    use unicode_segmentation::UnicodeSegmentation;
    for tool in tools.iter_mut() {
        let desc = &tool.tool_specification.description;
        let target_bytes = (desc.len() as f64 * ratio) as usize;
        let min_bytes = desc
            .grapheme_indices(true)
            .nth(MIN_DESCRIPTION_CHARS)
            .map(|(idx, _)| idx)
            .unwrap_or(desc.len());
        let target_bytes = target_bytes.max(min_bytes);
        if desc.len() > target_bytes {
            let truncate_at = desc
                .grapheme_indices(true)
                .take_while(|(idx, _)| *idx <= target_bytes)
                .last()
                .map(|(idx, g)| idx + g.len())
                .unwrap_or(0);
            tool.tool_specification.description = desc[..truncate_at].to_string();
        }
    }
}

/// 估算工具列表的总序列化大小
fn estimate_tools_size(tools: &[KiroTool]) -> usize {
    tools
        .iter()
        .map(|t| {
            let spec = &t.tool_specification;
            spec.name.len()
                + spec.description.len()
                + serde_json::to_string(&spec.input_schema.json)
                    .map(|s| s.len())
                    .unwrap_or(0)
        })
        .sum()
}

/// 简化工具的 input_schema
/// collapse_enums: 是否将 enum 折叠为摘要
fn simplify_schema(tool: &KiroTool, collapse_enums: bool) -> KiroTool {
    let schema = &tool.tool_specification.input_schema.json;
    let simplified = simplify_json_schema(schema, collapse_enums);

    KiroTool {
        tool_specification: ToolSpecification {
            name: tool.tool_specification.name.clone(),
            description: tool.tool_specification.description.clone(),
            input_schema: InputSchema::from_json(simplified),
        },
    }
}

/// 递归简化 JSON Schema
fn simplify_json_schema(schema: &serde_json::Value, collapse_enums: bool) -> serde_json::Value {
    let Some(obj) = schema.as_object() else {
        return schema.clone();
    };

    let mut result = serde_json::Map::new();

    for key in &["$schema", "type", "required", "additionalProperties"] {
        if let Some(v) = obj.get(*key) {
            result.insert(key.to_string(), v.clone());
        }
    }

    if let Some(serde_json::Value::Object(props)) = obj.get("properties") {
        let mut simplified_props = serde_json::Map::new();
        for (name, prop_schema) in props {
            if let Some(prop_obj) = prop_schema.as_object() {
                let mut simplified_prop = serde_json::Map::new();
                if let Some(ty) = prop_obj.get("type") {
                    simplified_prop.insert("type".to_string(), ty.clone());
                }
                // 递归嵌套 properties
                if let Some(nested_props) = prop_obj.get("properties") {
                    let mut nested_schema = serde_json::Map::new();
                    nested_schema.insert(
                        "type".to_string(),
                        serde_json::Value::String("object".to_string()),
                    );
                    nested_schema.insert("properties".to_string(), nested_props.clone());
                    if let Some(req) = prop_obj.get("required") {
                        nested_schema.insert("required".to_string(), req.clone());
                    }
                    if let Some(ap) = prop_obj.get("additionalProperties") {
                        nested_schema.insert("additionalProperties".to_string(), ap.clone());
                    }
                    let nested =
                        simplify_json_schema(&serde_json::Value::Object(nested_schema), collapse_enums);
                    if let Some(np) = nested.get("properties") {
                        simplified_prop.insert("properties".to_string(), np.clone());
                    }
                    if let Some(req) = nested.get("required") {
                        simplified_prop.insert("required".to_string(), req.clone());
                    }
                    if let Some(ap) = nested.get("additionalProperties") {
                        simplified_prop.insert("additionalProperties".to_string(), ap.clone());
                    }
                }
                // items
                if let Some(items) = prop_obj.get("items") {
                    simplified_prop
                        .insert("items".to_string(), simplify_json_schema(items, collapse_enums));
                }
                // enum 处理
                if let Some(e) = prop_obj.get("enum") {
                    if collapse_enums {
                        if let Some(arr) = e.as_array() {
                            let summary = format!("...{} options", arr.len());
                            simplified_prop.insert(
                                "enum".to_string(),
                                serde_json::Value::Array(vec![serde_json::Value::String(summary)]),
                            );
                        }
                    } else {
                        simplified_prop.insert("enum".to_string(), e.clone());
                    }
                }
                simplified_props.insert(name.clone(), serde_json::Value::Object(simplified_prop));
            } else {
                simplified_props.insert(name.clone(), prop_schema.clone());
            }
        }
        result.insert(
            "properties".to_string(),
            serde_json::Value::Object(simplified_props),
        );
    }

    serde_json::Value::Object(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(name: &str, desc: &str, schema: serde_json::Value) -> KiroTool {
        KiroTool {
            tool_specification: ToolSpecification {
                name: name.to_string(),
                description: desc.to_string(),
                input_schema: InputSchema::from_json(schema),
            },
        }
    }

    #[test]
    fn test_no_compression_under_threshold() {
        let tools = vec![make_tool(
            "test",
            "A short description",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let result = compress_tools_if_needed(&tools, "auto");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].tool_specification.description,
            "A short description"
        );
    }

    #[test]
    fn test_compression_triggers_over_threshold() {
        let long_desc = "x".repeat(2000);
        let tools: Vec<KiroTool> = (0..15)
            .map(|i| {
                make_tool(
                    &format!("tool_{}", i),
                    &long_desc,
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "param1": {"type": "string", "description": "A very long parameter description"},
                            "param2": {"type": "number", "description": "Another long description"}
                        }
                    }),
                )
            })
            .collect();

        let original_size = estimate_tools_size(&tools);
        assert!(original_size > THRESHOLD_L1);

        let result = compress_tools_if_needed(&tools, "auto");
        let compressed_size = estimate_tools_size(&result);
        assert!(compressed_size < original_size);
    }

    #[test]
    fn test_level2_removes_all_descriptions() {
        let tools = vec![make_tool(
            "test",
            "This description should be removed",
            serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        )];
        let result = compress_tools_if_needed(&tools, "level2");
        assert_eq!(result[0].tool_specification.description, "test");
    }

    #[test]
    fn test_level3_collapses_enums() {
        let tools = vec![make_tool(
            "test",
            "desc",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["read", "write", "append", "truncate", "exclusive"]
                    }
                }
            }),
        )];
        let result = compress_tools_if_needed(&tools, "level3");
        let props = result[0].tool_specification.input_schema.json.get("properties").unwrap();
        let mode_enum = props.get("mode").unwrap().get("enum").unwrap();
        let first = mode_enum.as_array().unwrap()[0].as_str().unwrap();
        assert!(first.contains("5 options"));
    }

    #[test]
    fn test_off_returns_unchanged() {
        let tools = vec![make_tool(
            "test",
            "x".repeat(50000).as_str(),
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let result = compress_tools_if_needed(&tools, "off");
        assert_eq!(result[0].tool_specification.description.len(), 50000);
    }
}
