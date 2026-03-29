# Code style

- No abbreviations in variable names (`match_length` not `ml`, `forward_len` not `fwd`)
- No static methods — use freestanding functions or instance methods
- No logic blocks inside boolean expressions — extract a function instead
- Variable names must be self-explanatory without context (`cur` not `off`)
- No meaningless suffixes or qualifiers — every word in a name must carry information
- Align naming with compress.rs conventions (`cur`, `candidate`, `literal_start`, `match_limit`)
- Extract repeated patterns into named functions
- Flatten nesting: prefer early returns/continues over deep `if` blocks
