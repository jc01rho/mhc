const SHARED_SUBSCRIPTION_ITEM_LIMIT_MARKER: &str =
    "User subscription sharing currently supports only";

pub fn is_shared_subscription_item_limit_message(message: &str) -> bool {
    message.contains(SHARED_SUBSCRIPTION_ITEM_LIMIT_MARKER)
}

pub fn shared_subscription_item_limit_message(provider: &str, original: &str) -> String {
    format!(
        "{original} ({provider} rejected the tool-call items this session replays under subscription sharing; switch this session to a model whose route accepts tool calls)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_and_annotates_the_shared_item_limit_reply() {
        let reply = "400 User subscription sharing currently supports only text items";
        assert!(is_shared_subscription_item_limit_message(reply));
        assert!(!is_shared_subscription_item_limit_message("rate limited"));
        let annotated = shared_subscription_item_limit_message("chatgpt-subscription", reply);
        assert!(annotated.starts_with(reply));
        assert!(annotated.contains("chatgpt-subscription rejected the tool-call items"));
    }
}
