### Abstract away operation execution logic from the server's running state - @DaleSeo PR #517

I abstracted the operation execution logic from the server's running state, following the pattern used in apps. This change helped me write tests and identify a subtle bug where the execute tool wasn't propagating the OTel context.
