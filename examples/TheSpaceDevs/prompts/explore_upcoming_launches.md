---
name: explore_upcoming_launches
description: "Research upcoming space launches and summarize key details"
arguments:
  - name: search
    description: "Optional term to filter the results by (e.g. rocket name, agency, mission). Applied by you to the returned list, not by the tool."
    required: false
---

Use the ListUpcomingLaunches tool to get upcoming launches (soonest first). If "{{search}}" is provided, keep only the launches that match it; otherwise summarize the whole list.

For each launch, summarize:
- Launch name and status
- Rocket and launch provider
- Mission description and orbit
- Launch window (net date)
- Launch pad location

If nothing matches "{{search}}", say so and summarize the soonest upcoming launches instead.
