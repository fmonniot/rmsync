#!/bin/sh

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
    --memory=256m
