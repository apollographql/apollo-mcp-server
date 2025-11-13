### Fix fragment field validation in schema tree shaking - @DaleSeo PR #471

Fixed "field not found" errors that occurred when loading operations containing GraphQL fragments (inline fragments or fragment spreads) on union types or interfaces. The schema tree shaking algorithm now correctly handles fragments by evaluating them against their specific type conditions.
