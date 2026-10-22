#!/bin/sh
# Stand-in for ssh in tests: drops ssh options and the host name, then runs
# the remote command here, the way sshd would hand it to a shell.
while [ $# -gt 0 ]; do
    case "$1" in
        -t) shift ;;
        -o) shift 2 ;;
        *) break ;;
    esac
done
shift
exec sh -c "$*"
