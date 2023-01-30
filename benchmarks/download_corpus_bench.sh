#!/bin/bash

mkdir bench_files
cd bench_files || exit
wget https://sun.aei.polsl.pl//~sdeor/corpus/silesia.zip
unzip ./silesia.zip
rm silesia.zip

