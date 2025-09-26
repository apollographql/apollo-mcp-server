### feat: adding config option for trace sampling - @alocay PR #366

Adding configuration option to sample traces. Can use the following options:
1. Ratio based samples (ratio >= 1 is always sample)
2. Always on
3. Always off

Defaults to always on if not provided.
