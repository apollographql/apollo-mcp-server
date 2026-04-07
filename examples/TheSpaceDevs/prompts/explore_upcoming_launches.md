---
name: explore_upcoming_launches
description: "Research upcoming space launches and summarize key details"
arguments:
  - name: search
    description: "Search term to filter launches (e.g. rocket name, agency, mission)"
    required: true
  - name: limit
    description: "Number of launches to return"
---

Use the SearchUpcomingLaunches tool to find upcoming launches matching "{{search}}"{{limit}}.

For each launch found, summarize:
- Launch name and status
- Rocket and launch provider
- Mission description and orbit
- Launch window (net date)
- Launch pad location

If no launches match, suggest broadening the search term.
