DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_type WHERE typname = 'submission_status') THEN
    CREATE TYPE submission_status AS ENUM (
      'inqueue',
      'processing',
      'accepted',
      'wronganswer',
      'timelimitexceeded',
      'compilationerror',
      'runtimeerrorsigsegv',
      'runtimeerrorsigxfsz',
      'runtimeerrorsigfpe',
      'runtimeerrorsigabrt',
      'runtimeerrornzec',
      'runtimeerrorother',
      'internalerror',
      'execformaterror'
    );
  END IF;
END$$;

CREATE TABLE IF NOT EXISTS submissions (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

  source_code TEXT NOT NULL,
  language TEXT NOT NULL,
  compiler_options TEXT,
  command_line_arguments TEXT,
  stdin TEXT,
  expected_output TEXT,

  cpu_time_limit DOUBLE PRECISION,
  cpu_extra_time DOUBLE PRECISION,
  wall_time_limit DOUBLE PRECISION,
  memory_limit DOUBLE PRECISION,
  stack_limit INTEGER,
  max_processes_and_or_threads INTEGER,

  enable_per_process_and_thread_time_limit BOOLEAN,
  enable_per_process_and_thread_memory_limit BOOLEAN,
  max_file_size INTEGER,
  redirect_stderr_to_stdout BOOLEAN,
  enable_network BOOLEAN,
  number_of_runs INTEGER,

  additional_files TEXT,
  callback_url TEXT,

  stdout TEXT,
  stderr TEXT,
  compile_output TEXT,
  message TEXT,

  exit_code INTEGER,
  exit_signal INTEGER,
  status submission_status NOT NULL,

  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  finished_at TIMESTAMPTZ,

  time DOUBLE PRECISION,
  wall_time DOUBLE PRECISION,
  memory DOUBLE PRECISION
);
