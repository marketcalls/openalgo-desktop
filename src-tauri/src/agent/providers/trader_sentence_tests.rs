//! A failed run reaches the trader as a sentence, never as the provider's
//! error (the web's test/test_agent_error_messages.py, ported against the
//! classifier in `agent::run::errors`): the same inputs, the same
//! sentences, and none of the provider's technical detail in any of them.

use crate::agent::run::errors::{
    provider_label, provider_message, ProviderFailure, INTERNAL_MESSAGE,
};

/// The failure a trader reported, with the key fragment replaced.
const OPENAI_BAD_KEY: &str = "litellm.AuthenticationError: AuthenticationError: OpenAIException - {\n  \"error\": {\n    \"message\": \"Incorrect API key provided: sk-proj-****************abcd. You can find your API key at https://platform.openai.com/account/api-keys.\",\n    \"type\": \"invalid_request_error\",\n    \"code\": \"invalid_api_key\",\n    \"param\": null\n  },\n  \"status\": 401\n}";

const TECHNICAL_FRAGMENTS: &[&str] = &[
    "sk-",
    "401",
    "litellm",
    "Exception",
    "http",
    "{",
    "invalid_api_key",
    "Error:",
];

fn readable(message: &str) {
    for fragment in TECHNICAL_FRAGMENTS {
        assert!(
            !message.contains(fragment),
            "{:?} reached the trader: {}",
            fragment,
            message
        );
    }
}

fn said(names: Vec<&'static str>, text: &str, provider: &str) -> String {
    let message = provider_message(
        &ProviderFailure {
            names,
            status: None,
            text: text.into(),
        },
        provider,
    );
    readable(&message);
    message
}

#[test]
fn each_failure_reads_as_the_webs_sentence() {
    assert_eq!(
        said(vec!["ModelProviderError"], OPENAI_BAD_KEY, ""),
        "OpenAI did not accept the API key for this model. Enter a valid key in agent settings."
    );
    assert_eq!(
        said(
            vec!["RateLimitError"],
            "litellm.RateLimitError: RateLimitError: OpenAIException - You exceeded your current quota, please check your plan and billing details. insufficient_quota",
            ""
        ),
        "Your OpenAI account has no credits left. Add credits under billing, then try again."
    );
    assert!(said(
        vec!["BadRequestError"],
        "litellm.BadRequestError: AnthropicException - Your credit balance is too low to access the API.",
        ""
    )
    .starts_with("Your Anthropic account has no credits left."));
    assert_eq!(
        said(
            vec!["RateLimitError"],
            "GroqException - Rate limit reached for requests",
            "groq"
        ),
        "Groq is limiting requests right now. Wait a minute, then try again."
    );
    assert_eq!(
        said(
            vec!["NotFoundError"],
            "OpenAIException - The model `gpt-9` does not exist or you do not have access to it.",
            ""
        ),
        "OpenAI does not offer the model chosen in agent settings. Choose another model there."
    );
    assert!(said(
        vec!["BadRequestError"],
        "OpenAIException - This model's maximum context length is 128000 tokens.",
        ""
    )
    .starts_with("This conversation has grown too long for the model."));
    assert_eq!(
        said(vec!["APIConnectionError"], "Connection error.", "anthropic"),
        "OpenAlgo could not reach Anthropic. Check that the server is online, then try again."
    );
    assert_eq!(
        said(
            vec!["ModelProviderError"],
            "AnthropicException - {\"type\": \"overloaded_error\"}",
            ""
        ),
        "Anthropic is having trouble right now. Try again in a few minutes."
    );
    assert_eq!(
        said(vec!["Timeout"], "Request timed out.", "gemini"),
        "Google Gemini took too long to answer. Try again."
    );
    assert_eq!(
        said(
            vec!["ModelProviderError"],
            "Error code: 503 - upstream said nothing useful",
            ""
        ),
        "The AI provider is having trouble right now. Try again in a few minutes."
    );
    assert_eq!(
        said(vec!["ModelProviderError"], "something odd happened", ""),
        "The AI provider could not complete this request. Try again, or check the model in agent settings."
    );
    assert!(!said(
        vec!["BadRequestError"],
        "OpenAIException - image input is not supported",
        ""
    )
    .contains("does not offer the model"));
}

#[test]
fn a_chatgpt_plan_failure_speaks_of_the_sign_in() {
    assert_eq!(
        said(vec!["AuthenticationError"], "ChatgptException - token expired", "chatgpt"),
        "Your ChatGPT sign-in has expired or was removed. Sign in to ChatGPT again from agent settings."
    );
    assert_eq!(
        said(
            vec!["BadRequestError"],
            "ChatgptException - The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account.",
            "chatgpt"
        ),
        "ChatGPT could not run the model chosen in agent settings. Choose another model there."
    );
}

#[test]
fn providers_are_named_from_the_run_or_the_text() {
    assert_eq!(
        provider_label("VertexAIException - quota", None),
        Some("Google Vertex AI")
    );
    assert_eq!(provider_label("", Some("openrouter")), Some("OpenRouter"));
    assert_eq!(provider_label("nothing to go on", None), None);
    readable(INTERNAL_MESSAGE);
    assert_eq!(
        INTERNAL_MESSAGE,
        "Something went wrong while answering. The details are in the error log. Try again."
    );
}
