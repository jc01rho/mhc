//! Port of senpi `look-at/tool-result-images.ts` (senpi#7260): an image a tool returned to a model
//! without image input is replaced by a vision model's description of it.

use maho_ai::types::is_video_mime_type;
use maho_ext_api::ToolContent;
use std::future::Future;

/// Every description is a request to the vision model, so a result with more images keeps the rest.
pub const MAX_DESCRIBED_IMAGES_PER_RESULT: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageDescription {
    /// `provider/id` of the vision model that wrote the description.
    pub model: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DescribedToolResult {
    /// The content with each described image replaced by its description; `None` when none was described.
    pub content: Option<Vec<ToolContent>>,
    /// Why each failed description failed. Its image stays where it was.
    pub failures: Vec<String>,
}

pub async fn describe_tool_result_images<F, Fut>(
    content: &[ToolContent],
    tool_name: &str,
    describe: F,
) -> DescribedToolResult
where
    F: Fn(&str, &str) -> Fut,
    Fut: Future<Output = Result<ImageDescription, String>>,
{
    let images: Vec<(usize, &str, &str)> = content
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ToolContent::Image { data, mime_type } if !is_video_mime_type(mime_type) => {
                Some((index, mime_type.as_str(), data.as_str()))
            }
            _ => None,
        })
        .take(MAX_DESCRIBED_IMAGES_PER_RESULT)
        .collect();
    let outcomes = futures::future::join_all(
        images
            .iter()
            .map(|(_, mime_type, data)| describe(mime_type, data)),
    )
    .await;
    let mut described = Vec::new();
    let mut failures = Vec::new();
    for ((index, _, _), outcome) in images.iter().zip(outcomes) {
        match outcome {
            Ok(description) => described.push((*index, description)),
            Err(reason) => failures.push(reason),
        }
    }
    if described.is_empty() {
        return DescribedToolResult {
            content: None,
            failures,
        };
    }
    let mut content = content.to_vec();
    for (index, description) in described {
        content[index] = ToolContent::text(crate::prompts::build_image_description_block(
            &description.model,
            tool_name,
            &description.text,
        ));
    }
    DescribedToolResult {
        content: Some(content),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(data: &str, mime_type: &str) -> ToolContent {
        ToolContent::Image {
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }

    fn described(model: &str, text: &str) -> ToolContent {
        ToolContent::text(crate::prompts::build_image_description_block(
            model, "look_at", text,
        ))
    }

    #[tokio::test]
    async fn described_images_take_their_place_and_failed_ones_stay() {
        let content = vec![
            ToolContent::text("before"),
            image("ok", "image/png"),
            image("bad", "image/png"),
            image("clip", "video/mp4"),
        ];
        let result = describe_tool_result_images(&content, "look_at", |_, data| {
            let data = data.to_owned();
            async move {
                match data.as_str() {
                    "ok" => Ok(ImageDescription {
                        model: "p/vision".into(),
                        text: "a chart".into(),
                    }),
                    other => Err(format!("cannot describe {other}")),
                }
            }
        })
        .await;
        assert_eq!(result.failures, vec!["cannot describe bad".to_owned()]);
        assert_eq!(
            result.content,
            Some(vec![
                ToolContent::text("before"),
                described("p/vision", "a chart"),
                image("bad", "image/png"),
                image("clip", "video/mp4"),
            ])
        );
    }

    #[tokio::test]
    async fn at_most_three_images_are_described_and_none_described_changes_nothing() {
        let content: Vec<ToolContent> = (0..5)
            .map(|index| image(&index.to_string(), "image/png"))
            .collect();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = describe_tool_result_images(&content, "look_at", |_, data| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let text = data.to_owned();
            async move {
                Ok(ImageDescription {
                    model: "p/vision".into(),
                    text,
                })
            }
        })
        .await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        let content = result.content.expect("described");
        assert_eq!(content[2], described("p/vision", "2"));
        assert_eq!(content[3], image("3", "image/png"));

        let failed =
            describe_tool_result_images(&[image("x", "image/png")], "look_at", |_, _| async {
                Err::<ImageDescription, _>("down".to_owned())
            })
            .await;
        assert_eq!(failed.content, None);
        assert_eq!(failed.failures, vec!["down".to_owned()]);
    }
}
