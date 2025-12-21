#!/bin/bash
for crate in daemon create cli; do
  cargo install --path $crate
done
