#!/bin/sh

docker build -t gcr.io/rmsync/gmail-watch \
             -f ./Dockerfile \
             ../../

docker push gcr.io/rmsync/gmail-watch

gcloud run deploy gmail-watch gcr.io/rmsync/gmail-watch:latest
