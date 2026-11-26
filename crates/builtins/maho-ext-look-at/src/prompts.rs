pub const LOOK_AT_SYSTEM_PROMPT:&str="You analyze attached media for a downstream agent that cannot inspect the attachments directly.

Extract only the information requested by the goal. Match the language of the goal.

Evidence rules:
- Be evidence-first. Clearly separate direct observations from inferences, and label inferences as such.
- Transcribe every visible piece of text verbatim, preserving casing, punctuation, and reading order. Mark unreadable text explicitly rather than guessing.
- Never fabricate details in occluded, blurry, cropped, or otherwise uncertain regions. State that the detail is unavailable or uncertain.
- For multiple attachments, report findings for each source label, then compare and contrast them when the goal calls for comparison.
- Be thorough about the goal and concise about unrelated detail.

Return only the response body. Do not add a preamble, meta commentary, or postscript.";
pub fn build_look_at_user_message(goal: &str, source_labels: &[String]) -> String {
    format!(
        "Goal:\n{goal}\n\nAttached sources:\n{}",
        source_labels
            .iter()
            .map(|label| format!("- {label}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}
/// The goal for an image a tool returned to a model that cannot see it; nobody asked a question about it yet.
pub const TOOL_IMAGE_DESCRIPTION_GOAL: &str = "Describe this image for an agent that cannot see it and has to judge whether it looks right. Say what it shows, how it is laid out, its colors, and every visible piece of text. Point out anything that looks broken, missing, misaligned, or out of place. Stay under 250 words.";
pub fn build_image_description_block(
    vision_model: &str,
    tool_name: &str,
    description: &str,
) -> String {
    format!(
        "[Image described by {vision_model} because this model cannot see images. For another question about it, call {tool_name} with the file path.]\n{description}"
    )
}
pub const LOOK_AT_DESCRIPTION: &str = "Extract basic information from media files such as PDFs, images, and diagrams when a quick summary is sufficient. Use it for simple text-based content extraction without precise visual reading. Do not use it for visual precision, aesthetic evaluation, or exact-accuracy work; use Read instead. Examples: extract all visible text verbatim as a bullet list in reading order; identify observations and the likely cause of a disabled Save button; list clearly visible product labels and their top, middle, or bottom shelf positions, marking unreadable labels as unreadable.";
pub const LOOK_AT_PROMPT_SNIPPET: &str = "Extract a quick summary or basic information from attached media; use Read instead when visual precision or exact accuracy matters.";
