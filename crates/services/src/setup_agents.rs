// SPDX-License-Identifier: Apache-2.0
//! Øphio's built-in agents: a reviewer for each profile Find & fix offers,
//! and the three people who use the app. Each is made from the brief Find &
//! fix gives that profile, so a task started from one reviews as Find & fix
//! would, but answers in the chat rather than filing issues. The three
//! people test the running app with the live-testing skill's method, which
//! is written once, in that skill.
use crate::github_after::{profile_brief, read_method};
use crate::launchers::{frontmatter, quoted_line};
use protocol::github::FindFocus;
use std::sync::OnceLock;

/// The built-in skill that teaches testing a running app.
pub(crate) const LIVE_TESTING: &str = include_str!("setup_skills/live-testing.md");

/// Each built-in agent's id, name, description and Find & fix profile.
const BUILT_IN: [(&str, &str, &str, Option<FindFocus>); 16] = [
    (
        "senior-engineer",
        "Senior engineer",
        "Reviews a project as a careful senior engineer joining it would: correctness first, then security, data safety and reliability, then tests, accessibility, performance and misleading docs. Use for a general review of a project, a folder or a change.",
        None,
    ),
    (
        "bug-hunter",
        "Bug hunter",
        "Hunts for bugs in the code: logic errors, edge cases, ignored errors, races and code that breaks what it or its docs promise. Use to find bugs before people do.",
        Some(FindFocus::Bugs),
    ),
    (
        "security-researcher",
        "Security researcher",
        "Reviews the code as a security researcher: injection, broken sign-in and access checks, leaked secrets, path traversal and unsafe defaults, reporting only what an attacker could reach. Use for a security review.",
        Some(FindFocus::Security),
    ),
    (
        "test-engineer",
        "Test engineer",
        "Reviews the tests for where they would let a real bug through: important behavior with no test, tests that can't fail, flaky tests and fixtures that hide bugs. Use before trusting a test suite.",
        Some(FindFocus::Tests),
    ),
    (
        "accessibility-specialist",
        "Accessibility specialist",
        "Reviews the interface's code for accessibility against WCAG 2.2 AA: names and roles, keyboard and screen reader use, focus, contrast, and target and text size. Use for an accessibility review.",
        Some(FindFocus::Accessibility),
    ),
    (
        "ui-ux-developer",
        "UI/UX developer",
        "Reviews the interface from its code as a UI/UX developer: confusing flows, missing loading, empty and error states, unclear controls, layouts that break and input that can be lost. Use for a design and usability review.",
        Some(FindFocus::UiUx),
    ),
    (
        "performance-engineer",
        "Performance engineer",
        "Reviews the code for performance at realistic sizes: repeated work, N+1 queries, blocking calls, unbounded memory and slow startup. Use when something is slow, or before it gets big.",
        Some(FindFocus::Performance),
    ),
    (
        "reliability-engineer",
        "Reliability engineer",
        "Reviews the code as the engineer on call: crashes, swallowed errors, lost or corrupted data, missing timeouts and failures part way through. Use before relying on it in real use.",
        Some(FindFocus::Reliability),
    ),
    (
        "code-quality-reviewer",
        "Code quality reviewer",
        "Reviews the code for what makes the next change risky: copies that drift apart, dead code, misleading names and leaky abstractions, not style. Use before a big change.",
        Some(FindFocus::Maintainability),
    ),
    (
        "dependency-auditor",
        "Dependency auditor",
        "Audits dependencies and the supply chain: vulnerable or abandoned packages, loose versions, and install scripts and CI steps that run unchecked code. Use for a dependency review.",
        Some(FindFocus::Dependencies),
    ),
    (
        "privacy-reviewer",
        "Privacy reviewer",
        "Reviews how the project handles personal data: what is collected, logged, shared or kept too long, left unencrypted or impossible to delete. Use for a privacy review.",
        Some(FindFocus::Privacy),
    ),
    (
        "api-reviewer",
        "API reviewer",
        "Reviews APIs and contracts: endpoints, functions, options and formats that don't match their docs or types, breaking changes and missing validation. Use before others build on it.",
        Some(FindFocus::Api),
    ),
    (
        "docs-reviewer",
        "Docs reviewer",
        "Checks the docs against the code as a newcomer following them word for word: instructions that no longer work, missing setup steps and behavior nobody wrote down. Use before a release or a new contributor.",
        Some(FindFocus::Docs),
    ),
    (
        "everyday-user",
        "Everyday user",
        "Tests the running app live as an everyday user: brings it up, uses it on the screen as a typical person would, and reports unclear steps, surprises, dead ends and anything broken. Use to see how an app works for its users.",
        Some(FindFocus::EverydayUser),
    ),
    (
        "novice-user",
        "Novice user",
        "Tests the running app live as someone who finds technology hard: reports jargon, hidden controls, alarming messages and anything that isn't simple. Use to check an app is easy enough for everyone.",
        Some(FindFocus::NoviceUser),
    ),
    (
        "power-user",
        "Power user",
        "Tests the running app live as a fast, technical person: counts clicks and waits, and reports needless steps and confirmations, missing shortcuts and anything that slows them down. Use to check an app for people who use it all day.",
        Some(FindFocus::PowerUser),
    ),
];

/// Each built-in agent's id and file, in the format Claude Code reads.
pub(crate) fn setup_agents() -> &'static [(&'static str, String)] {
    static AGENTS: OnceLock<Vec<(&'static str, String)>> = OnceLock::new();
    AGENTS.get_or_init(|| {
        BUILT_IN
            .iter()
            .map(|&(id, name, description, focus)| {
                let body = match focus {
                    Some(focus) if focus.uses_it() => tester(focus),
                    focus => reviewer(focus),
                };
                let text = format!(
                    "---\nname: {name}\ndescription: {}\n---\n\n{body}\n",
                    quoted_line(description)
                );
                (id, text)
            })
            .collect()
    })
}

/// The text of the built-in agent `id`.
pub(crate) fn setup_agent_text(id: &str) -> Option<&'static str> {
    setup_agents()
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, text)| text.as_str())
}

/// One who reads the source for what `focus` looks for.
fn reviewer(focus: Option<FindFocus>) -> String {
    format!(
        "Review the part of the project the person names; when they don't name one, review the whole project, starting with what changed most recently. Change nothing unless they ask you to fix what you find: then report first, and fix each problem with a test that fails before the fix and passes after it. Read-only commands that help, such as the test suite, are fine.\n\n\
         ## Your brief\n\n\
         {}\n\n\
         {}\n\n\
         ## Your answer\n\n\
         Answer in the chat, most serious first. For each finding give:\n\
         - a title that says the problem in plain words, as the person it affects would put it;\n\
         - what's wrong, and what should happen instead;\n\
         - where, as file and line;\n\
         - how to see it: the input or steps, and a test that would fail now, where you can write one;\n\
         - why it matters: who it hurts, and how;\n\
         - the likely fix when you are sure of it, or say that it is a guess;\n\
         - its severity and your confidence.\n\
         A few findings that are certainly real are worth more than many guesses. When you find nothing worth reporting, say so; don't pad the list.",
        profile_brief(focus),
        read_method(false),
    )
}

/// One who tests the running app as the person `focus` describes.
fn tester(focus: FindFocus) -> String {
    let (_, method) = frontmatter(LIVE_TESTING);
    format!(
        "Test the running app live, as the person described below, and report what they would meet. Try what the person who started this task asks; when they don't say, try the main tasks the app is for first, then the rest. Don't change the project's source: bring the app up from it as it is.\n\n\
         ## Persona\n\n\
         {}\n\n\
         ## Testing\n\n\
         {}\n\n\
         ## The persona in your report\n\n\
         Each problem also says how it felt, in one sentence in the persona's voice, and who it hurts: the people the persona stands for, and why it matters to them.",
        profile_brief(Some(focus)),
        method.trim(),
    )
}
