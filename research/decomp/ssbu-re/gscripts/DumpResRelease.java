import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionManager;
import java.io.FileWriter;
import java.io.PrintWriter;
import java.util.HashSet;
import java.util.Set;

public class DumpResRelease extends GhidraScript {
    private DecompInterface decompiler;
    private FunctionManager functions;
    private PrintWriter output;
    private final Set<Long> dumped = new HashSet<>();

    private Function ensure(long offset) throws Exception {
        Address address = toAddr(offset);
        Function function = functions.getFunctionContaining(address);
        if (function != null) {
            return function;
        }
        disassemble(address);
        return createFunction(address, null);
    }

    private void dump(long offset, String label) throws Exception {
        Function function = ensure(offset);
        if (function == null || !dumped.add(function.getEntryPoint().getOffset())) {
            return;
        }
        output.printf(
            "%n===== %s : %s @ %s =====%n",
            label,
            function.getName(),
            function.getEntryPoint()
        );
        DecompileResults result = decompiler.decompileFunction(function, 240, monitor);
        output.println(
            result != null && result.decompileCompleted()
                ? result.getDecompiledFunction().getC()
                : "DECOMPILE FAILED"
        );
    }

    @Override
    public void run() throws Exception {
        functions = currentProgram.getFunctionManager();
        decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        output = new PrintWriter(new FileWriter("resrelease_decomp.txt"));

        dump(0x3540560L, "resource_refcount_decrement");
        dump(0x60bfd8L, "fighter_effect_release_caller");
        dump(0x2608870L, "resource_release_caller_2608870");
        dump(0x353eb70L, "resource_release_caller_353eb70");
        dump(0x353eff0L, "queue_directory_file_releases");
        dump(0x3540860L, "directory_load_or_release");
        dump(0x3542ad0L, "directory_refcount_decrement");
        dump(0x3542b10L, "resource_worker_queue");
        dump(0x3542d20L, "directory_refcount_increment");
        dump(0x3542d80L, "collect_directory_file_paths");
        dump(0x355f8f0L, "load_effects");

        output.close();
        println("DumpResRelease done");
    }
}
