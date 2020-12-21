# Architecture Overview

Below is the workflow of the automatic email processor:

![overview](_media/gmail_overview.svg ':size=80%')

1. GMail will push a notification to Pub/Sub topic when a new email (or emails) is received
2. Cloud Pub/Sub will then call a Cloud Run endpoint with the payload. Cloud Run is google managed implementation of Kubernetes KNative. It is very similar to other Function as a Service offering, with the caveats that a docker image must be provided. Because Google Functions does not support Rust, we have to rely on this service.
3. Within our function, we call GMail to get the list of emails and their content
4. If there are any emails from fanfiction.net (or, in the future, other providers) we then retrieve the content and build an _epub_ version of it.
5. Finally, we upload the file to the remarkable cloud for consumption from the account's tablet.

?> The use of Cloud Run (or Functions) is in the name of cost. `rmsync` has been designed with a single user in mind. And even if that user does receive a lot of emails, it's still not enough to justify having a server running all the time :)

!> Do note that we currently do not support multi rmcloud users. This is however a relatively simple change to contributes if someone is interested in.