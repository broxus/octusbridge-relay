#!/usr/bin/env bash
set -eE

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_DIR="$SCRIPT_DIR/../"

function print_help() {
  echo 'Usage: generate.sh [OPTIONS]'
  echo ''
  echo 'Options:'
  echo '  -h,--help         Print this help message and exit'
  echo '  -t,--type TYPE    One of two types of installation:'
  echo '                    native - A little more complex way, but gives some'
  echo '                             performance gain and reduces the load.'
  echo '                    docker - The simplest way, but adds some overhead.'
  echo '                             Not recommended for machines with lower'
  echo '                             specs than required.'
  echo '  -i,--import       Import from existing phrases'
}

import="false"
while [[ $# -gt 0 ]]; do
  key="$1"
  case $key in
      -h|--help)
        print_help
        exit 0
      ;;
      -i|--import)
        import="true"
        shift # past argument
      ;;
      -t|--type)
        setup_type="$2"
        shift # past argument
        if [ "$#" -gt 0 ]; then shift;
        else
          echo 'ERROR: Expected installation type'
          echo ''
          print_help
          exit 1
        fi
      ;;
      *) # unknown option
        echo 'ERROR: Unknown option'
        echo ''
        print_help
        exit 1
      ;;
  esac
done

if [[ "$setup_type" != "native" ]] && [[ "$setup_type" != "docker" ]]; then
  echo 'ERROR: Unknown installation type'
  echo ''
  print_help
  exit 1
fi

if [[ "$setup_type" == "native" ]]; then
  relay_binary="sudo /usr/local/bin/relay generate"
elif [[ "$setup_type" == "docker" ]]; then
  if ! sudo docker info > /dev/null 2>&1; then
    echo 'ERROR: This script uses docker, and it is not running or not configured properly.'
    echo '       Please start docker and try again'
    exit 1
  fi

  relay_binary="sudo docker run -it --rm --mount type=bind,source=/etc/relay,target=/etc/relay relay generate"
else
  echo 'ERROR: Unexpected'
  exit 1
fi

if [[ "$import" == "true" ]]; then
  relay_binary="$relay_binary -i"
fi

bash -c "$relay_binary --config /etc/relay/config.yaml /etc/relay/keys.json"