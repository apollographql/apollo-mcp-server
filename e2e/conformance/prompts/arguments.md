---
name: test_prompt_with_arguments
description: A conformance fixture prompt with two required arguments.
arguments:
  - name: arg1
    required: true
  - name: arg2
    required: true
---
Prompt with arguments: arg1='{{arg1}}', arg2='{{arg2}}'
