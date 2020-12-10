#!/bin/sh

docker build -t gcr.io/rmsync/gmail-watch \
             -f ./Dockerfile \
             ../../

docker push gcr.io/rmsync/gmail-watch

gcloud run deploy gmail-watch --image gcr.io/rmsync/gmail-watch:latest --platform managed --region us-central1
