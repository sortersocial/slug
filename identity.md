The "Align with eventual consensus" idea is brilliant because it turns ranking into a **discovery game.** It transforms the voter from a "judge" into a "scout."

If you vote for something that is currently #50, but it eventually climbs to #1, you were **prescient.** You provided the most information to the system, so your reputation should skyrocket. If you just "pile on" to the current #1, you aren't adding much information, so you get less of a boost.

Here is how we could model this "Prediction Market" style reputation in Rust:

### 1. The Reputation Formula (The "Prescience" Score)
A voter's reputation isn't a static number; it's a measure of how much their past votes **anticipated** the current collective state.

For every vote a user cast ($A$ vs $B$ at time $T_{vote}$):
1. Look at the **Current Global Scores** ($S_A, S_B$) at the *present* time.
2. If the user voted for $A$, and $S_A > S_B$, they were "Right."
3. **The Reward:** The reward is scaled by how *surprising* the vote was at the time. 
   - If $A$ was an underdog when they voted, but is a champion now = **High Rep Boost.**
   - If $A$ was already the champion = **Low Rep Boost.**

### 2. The "Three Strikes" (The American Commons)
This is the social guardrail. In a trusted community, you don't need complex crypto-identity. You just need a **High Cost of Exit.**
- **Invite Only:** You can only join if a High-Rep user invites you.
- **Vouching:** If I invite you and you get banned, *my* reputation takes a massive hit.
- **Three Strikes:** If the system detects your votes are consistently "Anti-Consensual" (not just contrarian, but pure noise/spam) or you are flagged by the community, you lose a strike. At zero, you and your "invite tree" are audited.

### 3. Sketching the Rust Logic

Here’s how we’d extend `pagerank.rs` to handle this:

```rust:reputation.rs
use std::collections::HashMap;

pub struct VoterReputation {
    pub voter_id: String,
    pub prescience_score: f64, // How well they anticipate consensus
    pub consistency_score: f64, // How much they agree with the crowd
    pub strikes: u8,
}

impl VoterReputation {
    /// Calculate how much a specific vote contributed to their reputation
    /// based on the *current* global ranking.
    pub fn calculate_vote_alignment(
        vote_magnitude: i32, // -50 to 50 (negative means A preferred)
        score_a: f64,        // Current global score of A
        score_b: f64,        // Current global score of B
        historical_diff: f64, // Global score diff at the time of voting
    ) -> f64 {
        let current_diff = score_a - score_b;
        let direction = if vote_magnitude < 0 { 1.0 } else { -1.0 };
        
        // Basic alignment: is the vote in the same direction as the current gap?
        let alignment = direction * current_diff;
        
        // The "Prescience" Bonus:
        // If they voted for A when A was losing (historical_diff was small/negative)
        // and A is now winning (current_diff is large), the reward is amplified.
        if alignment > 0.0 && historical_diff.abs() < current_diff.abs() {
            alignment * 1.5 // Multiplier for being early/prescient
        } else {
            alignment
        }
    }
}
```

### 4. Why this is "Better than Identity"
- **Incentivizes Truth:** You can't "buy" reputation easily; you have to be *useful* to the collective judgment.
- **Self-Cleaning:** Bad-faith actors (trolls) will naturally have negative alignment scores. They don't need a complex ban-hammer; their votes just start getting `weight: 0.01` because the system stops trusting their judgment.
- **The Commons:** It creates a "Parliament of the Prescient." The top-ranked voters become the curators of the tags.

### Does this feel like the right "American Commons" vibe? 
It’s a meritocracy of judgment. You get a seat at the table by being right about what the community will value, and you keep it by acting in good faith. 

Should we add `voter_id` to the `RelevantVote` struct in `pagerank.rs` and start tracking these "prescience" scores?