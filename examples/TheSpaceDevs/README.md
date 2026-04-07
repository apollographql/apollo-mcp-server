# The Space Devs MCP Server

An Apollo MCP Server example for [The Space Devs](https://thespacedevs.com/) APIs, which expose
spaceflight information including launches, astronauts, and celestial bodies. The GraphQL endpoint
is hosted — no local server required.

## Running

```sh
cargo run -- examples/TheSpaceDevs/config.yaml
```

## Config variants

| Config | Transport | Operation source |
|--------|-----------|-----------------|
| `config.yaml` | `streamable_http` | `local` |
| `config-manifest.yaml` | `streamable_http` | `manifest` |
| `config-stdio.yaml` | `stdio` | `local` |

## Available operations

| File | Operation | Description |
|------|-----------|-------------|
| `operations/GetAstronautDetails.graphql` | `GetAstronautDetails` | Details for a single astronaut by ID |
| `operations/GetAstronautsCurrentlyInSpace.graphql` | `GetAstronautsCurrentlyInSpace` | Astronauts currently aboard the ISS |
| `operations/SearchUpcomingLaunches.graphql` | `SearchUpcomingLaunches` | Search upcoming rocket launches |
| `operations/ExploreCelestialBodies.graphql` | `ExploreCelestialBodies` | Browse planets, moons, and other bodies |

## Available prompts

To use the example prompts, copy them to the working directory where the server is started:

```sh
cp -r examples/TheSpaceDevs/prompts .
```

| File | Prompt | Description |
|------|--------|-------------|
| `prompts/astronaut_bio.md` | `astronaut_bio` | Look up an astronaut and write a short biography |
| `prompts/explore_upcoming_launches.md` | `explore_upcoming_launches` | Research upcoming space launches and summarize key details |
