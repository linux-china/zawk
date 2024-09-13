#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author $USER
# @meta version 0.1.0
# @var email user email
# @env DB_URL database url

BEGIN {
    print email, ENVIRON["DB_URL"]
}
