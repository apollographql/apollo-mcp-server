# Weather MCP Server

An Apollo MCP Server example using the [National Weather Service (NWS) API](https://api.weather.gov),
a free public API for US weather data. The GraphQL layer is built with Apollo Connectors.

## Running

`rover dev` spins up both the local router and the MCP server in one command:

```sh
cd examples/weather
rover dev --supergraph-config supergraph.yaml --mcp config.yaml
```

## Available operations

| File | Operation | Description |
|------|-----------|-------------|
| `operations/alerts.graphql` | `GetAlerts` | Active weather alerts for a US state |
| `operations/forecast.graphql` | `GetForecast` | Weather forecast for a coordinate |
| `operations/all.graphql` | `GetAllWeatherData` | Alerts and forecast in one query |
