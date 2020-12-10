#!/bin/sh

docker build -t gcr.io/rmsync/gmail-watch \
             -f ./Dockerfile \
             ../../
