//! Maps to CC `tools/WebSearchTool/prompt.ts`.

pub const WEB_SEARCH_TOOL_NAME: &str = "WebSearch";

pub fn get_web_search_prompt() -> String {
    // @nyan: once `nyan.web` is configured the nested Anthropic search tool is
    // never used, so the prompt must not describe it — even when no provider is
    // enabled, where the tool simply reports that none is configured.
    if crate::services::web::is_configured() {
        return "\n- Searches configured web providers and merges their results deterministically\n- Results are leads, not sources: fetch a page with WebFetch before citing it\n- Include a Sources section at the end of the response, listing only pages actually fetched with WebFetch\n- Domain filtering is supported; max_results defaults to 8 and is capped at 20\n- Provider errors and partial results are reported in the tool result\n".to_string();
    }
    // CC imports `getLocalMonthYear` from `constants/common.ts`.
    let current_month_year = crate::constants::common::get_local_month_year();
    format!(
        "\n- Allows you to search the web and use the results to inform responses\n- Provides up-to-date information for current events and recent data\n- Returns search result information formatted as search result blocks, including links as markdown hyperlinks\n- Use this tool for accessing information beyond the model's knowledge cutoff\n- Searches are performed automatically within a single API call\n\nCRITICAL REQUIREMENT - You MUST follow this:\n  - After answering the user's question, you MUST include a \"Sources:\" section at the end of your response\n  - In the Sources section, list all relevant URLs from the search results as markdown hyperlinks: [Title](URL)\n  - This is MANDATORY - never skip including sources in your response\n  - Example format:\n\n    [Your answer here]\n\n    Sources:\n    - [Source Title 1](https://example.com/1)\n    - [Source Title 2](https://example.com/2)\n\nUsage notes:\n  - Domain filtering is supported to include or block specific websites\n\nIMPORTANT - Use the correct year in search queries:\n  - The current month is {current_month_year}. You MUST use this year when searching for recent information, documentation, or current events.\n  - Example: If the user asks for \"latest React docs\", search for \"React documentation\" with the current year, NOT last year\n"
    )
}
