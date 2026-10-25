# Code style

- No abbreviations in variable names (`match_length` not `ml`, `forward_len` not `fwd`)
- No static methods — use freestanding functions or instance methods
- No logic blocks inside boolean expressions — extract a function instead
- Variable names must be self-explanatory without context (`cur` not `off`)
- Name variables the way you would describe them — if you'd say "can't beat the best match", name it `cant_beat_best`, not `dominated`
- No meaningless suffixes or qualifiers — every word in a name must carry information (e.g. don't say "local" when there is no non-local counterpart)
- Align naming with compress.rs conventions (`cur`, `candidate`, `literal_start`, `match_limit`)
- Extract repeated patterns into named functions
- Flatten nesting: prefer early returns/continues over deep `if` blocks — use guard clauses instead of `if { value } else { 0 }` patterns
