#!/bin/sh
# Serve draw.io on localhost:8080 for the plugin's draw.io renderer. The app
# runs this only when nothing answers at the renderer's URL, and stops it (with
# its process group) a while after the last draw.io view closed, or when the
# app quits. Stay in the foreground: exec, no `-d`.
#
# The first run pulls the image, hence the long timeout in plugin.json.
exec docker run --rm --name code-assistant-drawio -p 127.0.0.1:8080:8080 jgraph/drawio
