### Implement deduplication of operations - @DaleSeo PR #491

Fixed an issue where specifying both a directory and an explicit file path within that directory in the `operations.paths` configuration would create duplicate tools.
The server now automatically deduplicates operations based on their canonical file paths, ensuring that only one tool is created per unique operation file, regardless of how the paths are specified in the configuration.