# Staking Tokens

Penumbra's staking token, denoted `PEN`, represents units of unbonded stake.

Rather than treat bonded and unbonded stake as being two states of the same
token, Penumbra records stake bonded with a particular validator with a
*delegation token*, denoted `dPEN`.  Conversion between `PEN`
and `dPEN` occurs with an epoch-varying exchange rate that prices in what would
be a staking reward in other systems.  This ensures that all delegations to a
particular validator are fungible, and can be represented by a single token
representing an ownership share of that validator's delegation pool.

Stake bonded with different validators is not fungible, as different
validators may have different commission rates and different risk of
misbehavior.  Hence `dPEN` is a shorthand for a class of assets (one per
validator), rather than a single asset.  `dPEN` bonded to a specific
validator $v$ can be denoted `dPEN(v)` when it is necessary to be precise.

Each flavor of `dPEN` is its own first-class token, and like any other token
can be transferred between addresses, traded, sent over IBC, etc.  Penumbra
itself does not attempt to pool risk across validators, but nothing prevents
third parties from building stake pools composed of these assets.

The base reward rate for bonded stake is a parameter $r_e$ indexed by epoch.
This parameter can be thought of as a "Layer 1 Base Operating Rate", or
"L1BOR", in that it serves as a reference rate for the entire chain.  Its
value is set on a per-epoch basis by a formula involving the ratio of bonded
and unbonded stake, increasing when there is relatively less bonded stake and
decreasing when there is relatively more.  This formula should be decided and
adjusted by governance.

Each validator declares a set of funding streams, which comprise both the
destinations of their commission and the total commission rate $c_{v,e} \in
[0,1]$. $c{v,e}$ is subtracted from the base reward rate to get a
validator-specific reward rate $$r_{v,e} = (1 - c_{v,e})r_e.$$.

The base exchange rate between `PEN` and `dPEN` is given by the function
$$\psi(e) = \prod_{0 \leq i < e} (1 + r_i),$$ which measures the cumulative
depreciation of stake `PEN` relative to the delegation token `dPEN` from
genesis up to epoch $e$.  However, because `dPEN` is not a single asset but a
family of per-validator assets, this is only a base rate.

The actual exchange rate between stake `PEN` and validator $v$'s delegation
token `dPEN(v)` accounts for commissions by substituting the validator-specific
rate $r_{v,e}$ in place of the base rate $r$ to get $$\psi_v(e) = \prod_{0 \leq
i < e} (1 + r_{v,i}).$$

Delegating $x$ `PEN` to validator $v$ at epoch $e_1$ results in $x /
\psi_v(e_1)$ `dPEN`.  Undelegating $y$ `dPEN(v)` from validator $v$ at
epoch $e_2$ results in $y \psi_v(e_2)$ `PEN`.  Thus, delegating at epoch
$e_1$ and undelegating at epoch $e_2$ results in a return of $$\psi_v(e_2) /
\psi_v(e_1) = \prod_{e_1 \leq e < e_2} ( 1 + r_{v,e}),$$ i.e., the staking
reward compounded only over the period during which the stake was bonded.

Discounting newly bonded stake by the cumulative depreciation of unbonded
stake since genesis means that all bonded stake can be treated as if it had
been bonded since genesis, which allows newly unbonded stake to always be
inflated by the cumulative appreciation since genesis.  This mechanism avoids
the need to track the age of any particular delegation to compute its
rewards, and makes all shares of each validator's delegation pool fungible.
