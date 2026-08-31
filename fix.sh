#!/bin/bash
sed -i '/<<<<<<< HEAD/,/=======/{
  /<<<<<<< HEAD/d
  /=======/d
}
/>>>>>>> origin\/main/d' README.md
