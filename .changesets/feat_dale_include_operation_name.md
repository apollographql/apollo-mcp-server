### Include operation name with GraphQL requests - @DaleSeo PR #166

Include the operation name with GraphQL requests if it's available.

```diff
{
   "query":"query GetAlerts($state: String!) { alerts(state: $state) { severity description instruction } }",
   "variables":{
      "state":"CO"
   },
   "extensions":{
      "ApolloClientMetadata":{
         "type":"mcp",
         "version":"0.4.2"
      }
   },
+  "operationName":"GetAlerts"
}
```