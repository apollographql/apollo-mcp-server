---
default: minor
---

# Stop defaulting `deployment.environment.name` to `development`

Telemetry no longer sets `deployment.environment.name=development` when `ENVIRONMENT`
is unset. The attribute is now omitted unless `ENVIRONMENT` or `OTEL_RESOURCE_ATTRIBUTES`
sets it. Before, production deployments that didn't set `ENVIRONMENT` were reported as
`development`.

**Behavior change:** if your dashboards, monitors, or filters rely on `env:development`,
set the environment explicitly, for example
`OTEL_RESOURCE_ATTRIBUTES=deployment.environment.name=development`.

`service.name`, `service.version`, and `deployment.environment.name` now follow one
precedence rule: server config (`telemetry.service_name`, `telemetry.version`,
`ENVIRONMENT`) wins over `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`, which win
over defaults. Before, `OTEL_RESOURCE_ATTRIBUTES` silently overrode `telemetry.version`
and `ENVIRONMENT`. The server now logs a startup warning when the two sources disagree.
