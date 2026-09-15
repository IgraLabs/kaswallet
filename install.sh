#!/bin/bash
for crate in daemon create cli batch-send; do
  cargo install --locked --path $crate
done
