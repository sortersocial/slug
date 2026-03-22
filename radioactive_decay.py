#!/usr/bin/env python3
"""
Radioactive decay token emission analysis.
Verify the mathematical properties of different elements.
"""

import math

# Free parameters
TOTAL_SUPPLY = 1_000_000_000  # 1 billion tokens
CONTRIBUTOR_POOL_PCT = 0.961  # 96.1% of total supply
LIQUIDITY_POOL_PCT = 0.039    # 3.9% of total supply
LAUNCH_PRICE = 0.0001776      # dollars per token

# Elements defined by half-life in years
ELEMENTS = {
    "Promethium": 17.7,
    "Tritium": 12.32,
    "Cobalt-60": 5.27,
    "Californium-252": 2.65,
}

EPOCHS_PER_YEAR = 12  # monthly epochs

def calculate_decay_metrics(element_name, half_life_years):
    """Calculate emission rates and pool depletion for a given element."""

    half_life_months = half_life_years * 12

    # Decay constant: lambda = ln(2) / half_life
    decay_constant = math.log(2) / half_life_months

    # Emission rate per epoch (as a percentage of remaining pool)
    emission_rate_pct = decay_constant * 100

    # Initial contributor pool size
    initial_pool = TOTAL_SUPPLY * CONTRIBUTOR_POOL_PCT

    # Calculate pool remaining and cumulative emissions at key milestones
    milestones = {
        "Year 1": 12,
        "Year 3": 36,
        "Year 5": 60,
        "Year 10": 120,
        f"Half-life ({half_life_years}y)": int(half_life_months),
        "Year 30": 360,
        "Year 50": 600,
    }

    results = {
        "element": element_name,
        "half_life_years": half_life_years,
        "emission_rate_pct": emission_rate_pct,
        "decay_constant": decay_constant,
        "initial_pool": initial_pool,
        "milestones": {},
    }

    # First epoch emission (founder gets 90% of early emissions)
    pool_after_epoch_0 = initial_pool
    epoch_1_emission = pool_after_epoch_0 * decay_constant
    founder_epoch_1 = epoch_1_emission * 0.90
    founder_epoch_1_usd = founder_epoch_1 * LAUNCH_PRICE

    results["epoch_1_emission"] = epoch_1_emission
    results["founder_epoch_1_tokens"] = founder_epoch_1
    results["founder_epoch_1_usd"] = founder_epoch_1_usd

    # Calculate milestones
    cumulative_emitted = 0
    pool_remaining = initial_pool

    for i in range(1, max(milestones.values()) + 1):
        # In exponential decay: P(t) = P₀ × e^(-λt)
        # For discrete epochs: P(n) = P₀ × (1 - λ)^n
        pool_remaining = initial_pool * ((1 - decay_constant) ** i)
        cumulative_emitted = initial_pool - pool_remaining

        for label, epoch_count in milestones.items():
            if i == epoch_count:
                pool_pct = (pool_remaining / initial_pool) * 100
                emitted_pct = (cumulative_emitted / initial_pool) * 100
                results["milestones"][label] = {
                    "epochs": i,
                    "pool_remaining_tokens": pool_remaining,
                    "pool_remaining_pct": pool_pct,
                    "cumulative_emitted_tokens": cumulative_emitted,
                    "cumulative_emitted_pct": emitted_pct,
                }

    return results

def print_element_analysis(metrics):
    """Print formatted analysis for a single element."""
    print(f"\n{'='*70}")
    print(f"{metrics['element'].upper()} (Half-life: {metrics['half_life_years']} years)")
    print(f"{'='*70}")

    print(f"\nBasic Parameters:")
    print(f"  Decay constant (λ):        {metrics['decay_constant']:.6f} per month")
    print(f"  Emission rate per epoch:   {metrics['emission_rate_pct']:.4f}%")
    print(f"  Initial pool size:         {metrics['initial_pool']:,.0f} tokens")

    print(f"\nEpoch 1 (First Month):")
    print(f"  Total emission:            {metrics['epoch_1_emission']:,.0f} tokens")
    print(f"  Founder share (90%):       {metrics['founder_epoch_1_tokens']:,.0f} tokens")
    print(f"  Founder earnings (USD):    ${metrics['founder_epoch_1_usd']:,.2f}")

    print(f"\nPool Depletion Over Time:")
    print(f"  {'Milestone':<25} {'Pool Remaining':<18} {'Emitted':<18}")
    print(f"  {'-'*61}")

    for label in sorted(metrics["milestones"].keys(),
                       key=lambda x: metrics["milestones"][x]["epochs"]):
        m = metrics["milestones"][label]
        pool_pct = m["pool_remaining_pct"]
        emitted_pct = m["cumulative_emitted_pct"]
        print(f"  {label:<25} {pool_pct:>6.1f}%           {emitted_pct:>6.1f}%")

def compare_elements():
    """Compare all elements side by side."""
    print(f"\n{'='*80}")
    print("ELEMENT COMPARISON")
    print(f"{'='*80}")

    all_metrics = {}
    for element_name, half_life in sorted(ELEMENTS.items(),
                                          key=lambda x: x[1], reverse=True):
        metrics = calculate_decay_metrics(element_name, half_life)
        all_metrics[element_name] = metrics
        print_element_analysis(metrics)

    # Side-by-side comparison table
    print(f"\n{'='*80}")
    print("QUICK COMPARISON TABLE")
    print(f"{'='*80}")

    print(f"\n{'Element':<20} {'Year 1':<12} {'Year 5':<12} {'Year 10':<12} {'Half-life Pool':<12}")
    print(f"{'-'*68}")

    for element_name in sorted(all_metrics.keys(),
                               key=lambda x: all_metrics[x]["half_life_years"],
                               reverse=True):
        m = all_metrics[element_name]
        y1 = m["milestones"]["Year 1"]["pool_remaining_pct"]
        y5 = m["milestones"]["Year 5"]["pool_remaining_pct"]
        y10 = m["milestones"]["Year 10"]["pool_remaining_pct"]
        hl = m["milestones"][f"Half-life ({m['half_life_years']}y)"]["pool_remaining_pct"]

        print(f"{element_name:<20} {y1:>6.1f}%     {y5:>6.1f}%     {y10:>6.1f}%     {hl:>6.1f}%")

if __name__ == "__main__":
    compare_elements()
