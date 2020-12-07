# Authorization Cloud Function

Imported from https://github.com/googlecodelabs/gcf-gmail-codelab/tree/master/auth and tweaked to
this project needs.

At some point in the future, this project will likely be rewritten in rust.

Take care to not commit the `env_vars.yaml` file with credentials in it. By default git will
ignore this file (you'll have to create it from `env_vars.yaml.default`).

## Deployment

Assuming the Google Cloud SDK is installed, configured and pointing to the desired project.
The following commands will deploy the two auth functions:

```bash
gcloud functions deploy auth_init \
    --runtime=nodejs10 \
    --trigger-http \
    --env-vars-file=env_vars.yaml \
    --allow-unauthenticated \
    --memory=128m

gcloud functions deploy auth_callback \
    --runtime=nodejs10 \
    --trigger-http \
    --env-vars-file=env_vars.yaml \
    --allow-unauthenticated \
    --memory=128m
```

## License

The code in this directory is licensed under the Apache 2.0 license.
