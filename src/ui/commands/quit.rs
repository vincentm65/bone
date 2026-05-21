// Quit is handled directly in the dispatch table (returns CommandResult::Quit).
// This module exists so the file structure mirrors the other commands and
// future quit logic (cleanup, confirmations, etc.) has a natural home.
