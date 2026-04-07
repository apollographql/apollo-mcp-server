---
name: astronaut_bio
description: "Look up an astronaut and write a short biography"
arguments:
  - name: name
    description: "The astronaut's name to search for"
    required: true
---

1. Use the SearchUpcomingLaunches or available search tools to find an astronaut named "{{name}}"
2. Once you have their ID, use GetAstronautDetails to get their full profile
3. Write a concise biography covering:
   - Full name, nationality, and date of birth
   - Agency and current status
   - Number of spaceflights and total time in space
   - Notable missions or achievements
   - Current role or last known activity
