# `compress_hc.rs` high-level algorithm overview

This file contains **three different high-compression strategies** behind the same public API.

At a high level, all three produce normal LZ4 sequences:

- a run of **literals**
- followed by a **match**
  - an `offset`
  - a `match length`

What changes between the strategies is **how hard they search for matches** and **how they choose between competing matches**.

## Strategy selection

`compress_hc()` first clamps the level and maps it to one of these strategies:

- **levels 0-2**: `Mid`
- **levels 3-9**: `HashChain`
- **levels 10-12**: `Optimal`

That mapping is defined by `hc_level_params()`.

---

## 1. Mid strategy

**Used for levels 0-2**

### Core idea

This is the lightweight HC mode.

It keeps two plain hash tables:

- one keyed by **4-byte sequences**
- one keyed by **8-byte sequences**

Each table stores only the **most recent** position for a hash.

So unlike the full hash-chain HC mode, it does **not** keep a long linked history of older candidates.

### How it works

At each position:

1. Try the **8-byte hash** first
2. If that does not produce a usable match, try the **4-byte hash**
3. If a match is found:
   - extend it backward if possible
   - extend it forward as far as bytes keep matching
   - emit it immediately
4. If no match is found, move forward with a small acceleration rule

There is also a small amount of local lookahead around the 4-byte path, but the strategy is still mostly greedy.

---

## 2. Hash-chain HC strategy

**Used for levels 3-9**

### Core idea

Instead of remembering only the latest candidate for each hash, it keeps:

- a **dictionary**: the most recent position for each hash
- a **chain table**: a backward link from each position to an older position with the same hash

That means each hash bucket behaves like a short linked list of candidate matches.

### How it works

At each position:

1. Insert the current position into the hash/chain structure
2. Use the current 4-byte prefix to find the newest candidate
3. Walk backward through older candidates in the chain
4. For each candidate:
   - reject impossible or weak candidates quickly
   - measure the actual match length
   - keep the best local match seen so far
5. After finding `current_match`, run another hash-chain search near `current_match.end() - 2` to see whether a better follow-up match exists
6. If the follow-up match overlaps the current one, shorten or trim the matches so the emitted sequence stays valid, then emit

The compression level controls the chain search budget through `max_attempts`.

### Important difference from Mid

Mid asks:

> Is there a good recent match here?

Hash-chain HC asks:

> Among several older matches with the same hash, which one gives the best local result?

### Lazy evaluation

This strategy does not always emit the first acceptable match immediately.

That is what is **lazy** here: after finding `current_match`, it runs another search near `current_match.end() - 2` before committing. If that second search finds a better follow-up match, it may prefer that one instead.

Two matches **overlap** when they cover some of the same input bytes. Since both cannot be emitted unchanged, the parser may shorten the earlier match or trim the front of the later one before emitting.

### Decision style

This mode is:

- **deeper search** than Mid
- still mostly **local**
- smarter about **nearby competing matches**

### Mental model

> Search several older candidates, keep the best local match, then do a little lookahead before committing.

---

## 3. Optimal parsing strategy

**Used for levels 10-12**

### Core idea

This mode still uses strong hash-chain-based match finding, but the decision rule changes completely.

Instead of asking:

> What is the best match right now?

it asks:

> What sequence of literals and matches gives the cheapest encoded output over the next window?

So this is a small **dynamic-programming parser**.

### How it works

At a position:

1. Find a strong initial match
2. If that match is obviously good enough, emit it immediately as a fast path
3. Otherwise, build an optimal parse window
4. For each reachable position in that window, track the best known encoding cost to get there
5. Refine those costs by exploring additional matches from intermediate positions
6. Reverse the winning path and emit the chosen sequence of literals and matches

The DP state is stored in the `OptimalState` array.

### Cost model

The parser compares the byte cost of choices using helper functions such as:

- `literals_price()`
- `sequence_price()`

So it is explicitly choosing the path with the best compressed size inside the lookahead window.

### Level differences

Within optimal mode:

- **level 10**: moderate search
- **level 11**: deeper search
- **level 12**: most exhaustive update/search behavior

### Decision style

This mode is:

- the **slowest** of the three
- the **best ratio** of the three
- the only one doing explicit **cost-based path optimization**

### Mental model

> Draft several possible futures, score them by encoded size, and choose the cheapest path.

---

## Quick comparison

| Strategy | Levels | Match search | Match choice | Speed | Ratio |
|---|---:|---|---|---|---|
| Mid | 0-2 | Very shallow | Greedy/local | Fastest of the three | Lowest of the three |
| Hash-chain HC | 3-9 | Deep chain walk | Local + lazy overlap resolution | Middle | Better |
| Optimal | 10-12 | Deep chain walk | Dynamic programming over a window | Slowest | Best |

---

## How to read `compress_hc.rs`

If you want to read the file top-down, this is the main story:

1. `compress_hc()`
   - chooses the strategy from the level
2. `compress_mid_internal()`
   - dual-hash, mostly greedy compression
3. `compress_hash_chain_internal()`
   - hash-chain search plus lazy local match resolution
4. `compress_opt_internal()`
   - optimal parsing with a dynamic-programming window

That is the main conceptual split in the file.
