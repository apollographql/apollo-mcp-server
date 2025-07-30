### fix: Add custom deserializer to handle APOLLO_UPLINK_ENDPOINTS environment variable parsing - @swcollard PR #220

The APOLLO_UPLINK_ENDPOINTS environment variables has historically been a comma separated list of URL strings.
The move to yaml configuration allows us to more directly define the endpoints as a Vec.
This fix introduces a custom deserializer for the `apollo_uplink_endpoints` config field that can handle both the environment variable comma separated string, and the yaml-based list.